use std::ffi::{CStr, CString};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;

use genkan_lock_auth::{Connection, Message, Secret, MAX_MESSAGES, MAX_MESSAGE_BYTES};
use pam_sys2::{
    pam_authenticate, pam_conv, pam_end, pam_get_user, pam_handle_t, pam_message, pam_response,
    pam_start, PAM_CONV_ERR, PAM_DISALLOW_NULL_AUTHTOK, PAM_ERROR_MSG, PAM_MAX_NUM_MSG,
    PAM_PROMPT_ECHO_OFF, PAM_PROMPT_ECHO_ON, PAM_SUCCESS, PAM_TEXT_INFO,
};
use zeroize::Zeroize;

const SERVICE: &str = "genkan-lock";

fn main() {
    if run().is_err() {
        std::process::exit(1);
    }
}

fn run() -> Result<(), ()> {
    let fd = parse_fd()?;
    // SAFETY: this private binary is spawned with exclusive ownership of the
    // inherited descriptor, which is converted exactly once.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    let uid = unsafe { libc::getuid() };
    if unsafe { libc::geteuid() } != uid || unsafe { libc::getegid() } != unsafe { libc::getgid() }
    {
        return Err(());
    }
    let username = username_for_uid(uid).ok_or(())?;
    let mut connection = Connection::new(stream);
    connection
        .send(&Message::Ready {
            uid,
            username: username.clone(),
        })
        .map_err(|_| ())?;

    loop {
        let mut conversation = PamConversation {
            connection: &mut connection,
            next_id: 1,
            cancelled: false,
        };
        if authenticate(&username, uid, &mut conversation) && !conversation.cancelled {
            return connection.send(&Message::Success).map_err(|_| ());
        }
        connection.send(&Message::Failure).map_err(|_| ())?;
        let message = connection.receive().map_err(|_| ())?;
        match message {
            Message::Retry => {}
            Message::Cancel => return Ok(()),
            _ => return Err(()),
        }
    }
}

fn parse_fd() -> Result<RawFd, ()> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let flag = arguments.next().ok_or(())?;
    let value = arguments.next().ok_or(())?;
    if flag != "--fd" || arguments.next().is_some() {
        return Err(());
    }
    let value = value.to_str().ok_or(())?.parse::<RawFd>().map_err(|_| ())?;
    (value > 2).then_some(value).ok_or(())
}

struct PamConversation<'a> {
    connection: &'a mut Connection,
    next_id: u64,
    cancelled: bool,
}

impl PamConversation<'_> {
    fn prompt(&mut self, prompt: &CStr, secret: bool) -> Result<Secret, ()> {
        if self.next_id > MAX_MESSAGES {
            return Err(());
        }
        let id = self.next_id;
        self.next_id += 1;
        let text = bounded_message(prompt);
        self.connection
            .send(&Message::Prompt { id, secret, text })
            .map_err(|_| ())?;
        match self.connection.receive().map_err(|_| ())? {
            Message::Response {
                id: response_id,
                value,
            } if response_id == id => Ok(value),
            Message::Cancel => {
                self.cancelled = true;
                Err(())
            }
            _ => Err(()),
        }
    }

    fn notice(&mut self, message: &CStr, error: bool) {
        let message = if error {
            Message::Error(bounded_message(message))
        } else {
            Message::Info(bounded_message(message))
        };
        let _ = self.connection.send(&message);
    }
}

fn authenticate(username: &str, uid: u32, conversation: &mut PamConversation<'_>) -> bool {
    let Ok(service) = CString::new(SERVICE) else {
        return false;
    };
    let Ok(username) = CString::new(username) else {
        return false;
    };
    let mut handle = std::ptr::null_mut();
    let pam_conversation = pam_conv {
        conv: Some(pam_conversation),
        appdata_ptr: std::ptr::from_mut(conversation).cast(),
    };
    // SAFETY: all C strings, callback state, and output storage remain valid
    // through pam_end below.
    let started = unsafe {
        pam_start(
            service.as_ptr(),
            username.as_ptr(),
            &pam_conversation,
            &mut handle,
        )
    };
    if started != PAM_SUCCESS || handle.is_null() {
        return false;
    }
    // SAFETY: pam_start returned an initialized handle.
    let status = unsafe { pam_authenticate(handle, PAM_DISALLOW_NULL_AUTHTOK) };
    let same_user = status == PAM_SUCCESS && pam_uid(handle).is_some_and(|found| found == uid);
    // SAFETY: the initialized handle is ended exactly once. Treat teardown
    // failure as authentication failure rather than authorizing from a PAM
    // transaction whose terminal state is uncertain.
    let ended = unsafe { pam_end(handle, status) };
    same_user && ended == PAM_SUCCESS
}

unsafe extern "C" fn pam_conversation(
    count: libc::c_int,
    messages: *mut *const pam_message,
    responses: *mut *mut pam_response,
    data: *mut libc::c_void,
) -> libc::c_int {
    if !responses.is_null() {
        // SAFETY: PAM supplied writable output storage for the callback.
        unsafe { *responses = std::ptr::null_mut() };
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        pam_conversation_inner(count, messages, responses, data)
    }))
    .unwrap_or(PAM_CONV_ERR)
}

unsafe fn pam_conversation_inner(
    count: libc::c_int,
    messages: *mut *const pam_message,
    responses: *mut *mut pam_response,
    data: *mut libc::c_void,
) -> libc::c_int {
    if count <= 0
        || count > PAM_MAX_NUM_MSG
        || messages.is_null()
        || responses.is_null()
        || data.is_null()
    {
        return PAM_CONV_ERR;
    }
    // SAFETY: PAM guarantees an array of `count` message pointers and the
    // callback data points to the live worker conversation.
    let messages = unsafe { std::slice::from_raw_parts(messages, count as usize) };
    let bytes = count as usize * std::mem::size_of::<pam_response>();
    // SAFETY: calloc creates a C-owned, zeroed response array PAM may free.
    let allocated = unsafe { libc::calloc(1, bytes) }.cast::<pam_response>();
    if allocated.is_null() {
        return PAM_CONV_ERR;
    }
    let mut allocated = PendingResponses {
        pointer: allocated,
        count: count as usize,
    };
    // SAFETY: validated non-null callback data has the original type.
    let conversation = unsafe { &mut *data.cast::<PamConversation<'_>>() };
    for (index, message) in messages.iter().enumerate() {
        if message.is_null() || unsafe { (**message).msg }.is_null() {
            return PAM_CONV_ERR;
        }
        // SAFETY: PAM message text is NUL-terminated for the callback.
        let text = unsafe { CStr::from_ptr((**message).msg) };
        let style = unsafe { (**message).msg_style };
        match style {
            PAM_PROMPT_ECHO_ON | PAM_PROMPT_ECHO_OFF => {
                let Ok(mut secret) = conversation.prompt(text, style == PAM_PROMPT_ECHO_OFF) else {
                    return PAM_CONV_ERR;
                };
                let length = secret.expose().len();
                // SAFETY: allocate one trailing NUL for the PAM-owned response.
                let response = unsafe { libc::malloc(length + 1) }.cast::<u8>();
                if response.is_null() {
                    secret.zeroize();
                    return PAM_CONV_ERR;
                }
                // SAFETY: source and destination are valid, disjoint buffers.
                unsafe {
                    std::ptr::copy_nonoverlapping(secret.expose().as_ptr(), response, length);
                    *response.add(length) = 0;
                }
                secret.zeroize();
                // SAFETY: the guard owns `count` initialized response slots.
                let output = unsafe { allocated.as_mut() };
                output[index].resp = response.cast();
                output[index].resp_retcode = 0;
            }
            PAM_TEXT_INFO => conversation.notice(text, false),
            PAM_ERROR_MSG => conversation.notice(text, true),
            _ => return PAM_CONV_ERR,
        }
    }
    // SAFETY: PAM owns this complete response array after success.
    unsafe { *responses = allocated.release() };
    PAM_SUCCESS
}

struct PendingResponses {
    pointer: *mut pam_response,
    count: usize,
}

impl PendingResponses {
    unsafe fn as_mut(&mut self) -> &mut [pam_response] {
        // SAFETY: this guard was constructed from a calloc allocation of
        // exactly `count` response entries and retains exclusive ownership.
        unsafe { std::slice::from_raw_parts_mut(self.pointer, self.count) }
    }

    fn release(mut self) -> *mut pam_response {
        std::mem::replace(&mut self.pointer, std::ptr::null_mut())
    }
}

impl Drop for PendingResponses {
    fn drop(&mut self) {
        if self.pointer.is_null() {
            return;
        }
        // SAFETY: the guard exclusively owns this response allocation. This
        // also runs while unwinding from the callback's protected Rust body.
        unsafe {
            clear_responses(self.as_mut());
            libc::free(self.pointer.cast());
        }
    }
}

unsafe fn clear_responses(responses: &mut [pam_response]) {
    for response in responses {
        if !response.resp.is_null() {
            // SAFETY: locally allocated responses are NUL-terminated.
            let length = unsafe { libc::strlen(response.resp) };
            // SAFETY: overwrite the allocation before releasing it.
            unsafe {
                std::ptr::write_bytes(response.resp.cast::<u8>(), 0, length);
                libc::free(response.resp.cast())
            };
            response.resp = std::ptr::null_mut();
        }
    }
}

fn pam_uid(handle: *mut pam_handle_t) -> Option<u32> {
    let mut username = std::ptr::null();
    // SAFETY: handle is live and output storage is valid.
    if unsafe { pam_get_user(handle, &mut username, std::ptr::null()) } != PAM_SUCCESS
        || username.is_null()
    {
        return None;
    }
    // SAFETY: PAM returned a NUL-terminated user name for this live handle.
    username_uid(unsafe { CStr::from_ptr(username) }.to_bytes())
}

fn bounded_message(message: &CStr) -> String {
    let message = String::from_utf8_lossy(message.to_bytes());
    let mut result = message.chars().take(MAX_MESSAGE_BYTES).collect::<String>();
    while result.len() > MAX_MESSAGE_BYTES {
        result.pop();
    }
    result
}

fn username_for_uid(uid: u32) -> Option<String> {
    let mut buffer = vec![0_u8; 16 * 1024];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // SAFETY: all output storage remains live for this call.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: successful NSS lookup initialized the entry and its NUL-terminated fields.
    let entry = unsafe { entry.assume_init() };
    // SAFETY: pw_name points into the live NSS buffer.
    unsafe { CStr::from_ptr(entry.pw_name) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

fn username_uid(username: &[u8]) -> Option<u32> {
    let username = CString::new(username).ok()?;
    let mut buffer = vec![0_u8; 16 * 1024];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // SAFETY: all input and output storage remains live for this call.
    let status = unsafe {
        libc::getpwnam_r(
            username.as_ptr(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: successful NSS lookup initialized the entry.
    Some(unsafe { entry.assume_init() }.pw_uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_a_distinct_response_for_each_pam_prompt() {
        let (worker, ui) = UnixStream::pair().unwrap();
        let mut worker = Connection::new(worker);
        let mut conversation = PamConversation {
            connection: &mut worker,
            next_id: 1,
            cancelled: false,
        };
        let mut ui = Connection::new(ui);
        let responder = std::thread::spawn(move || {
            assert_eq!(
                ui.receive().unwrap(),
                Message::Prompt {
                    id: 1,
                    secret: false,
                    text: "Login".into()
                }
            );
            ui.send(&Message::Response {
                id: 1,
                value: Secret::new(b"alice".to_vec()).unwrap(),
            })
            .unwrap();
            assert_eq!(
                ui.receive().unwrap(),
                Message::Prompt {
                    id: 2,
                    secret: true,
                    text: "Password".into()
                }
            );
            ui.send(&Message::Response {
                id: 2,
                value: Secret::new(b"correct horse".to_vec()).unwrap(),
            })
            .unwrap();
        });

        assert_eq!(
            conversation.prompt(c"Login", false).unwrap().expose(),
            b"alice"
        );
        assert_eq!(
            conversation.prompt(c"Password", true).unwrap().expose(),
            b"correct horse"
        );
        responder.join().unwrap();
    }

    #[test]
    fn cancellation_invalidates_the_pam_attempt() {
        let (worker, ui) = UnixStream::pair().unwrap();
        let mut worker = Connection::new(worker);
        let mut conversation = PamConversation {
            connection: &mut worker,
            next_id: 1,
            cancelled: false,
        };
        let mut ui = Connection::new(ui);
        let responder = std::thread::spawn(move || {
            assert!(matches!(
                ui.receive().unwrap(),
                Message::Prompt { id: 1, .. }
            ));
            ui.send(&Message::Cancel).unwrap();
        });

        assert!(conversation.prompt(c"Password", true).is_err());
        assert!(conversation.cancelled);
        responder.join().unwrap();
    }

    #[test]
    fn bounds_lossy_pam_messages_without_splitting_utf8() {
        let message = CString::new("界".repeat(MAX_MESSAGE_BYTES)).unwrap();
        let bounded = bounded_message(&message);
        assert!(bounded.len() <= MAX_MESSAGE_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn preserves_mixed_pam_message_arrays_and_response_indexes() {
        let (worker, ui) = UnixStream::pair().unwrap();
        let mut worker = Connection::new(worker);
        let mut conversation = PamConversation {
            connection: &mut worker,
            next_id: 1,
            cancelled: false,
        };
        let responder = std::thread::spawn(move || {
            let mut ui = Connection::new(ui);
            assert_eq!(ui.receive().unwrap(), Message::Info("Information".into()));
            assert!(matches!(
                ui.receive().unwrap(),
                Message::Prompt {
                    id: 1,
                    secret: true,
                    ..
                }
            ));
            ui.send(&Message::Response {
                id: 1,
                value: Secret::new(b"first".to_vec()).unwrap(),
            })
            .unwrap();
            assert_eq!(ui.receive().unwrap(), Message::Error("Warning".into()));
            assert!(matches!(
                ui.receive().unwrap(),
                Message::Prompt {
                    id: 2,
                    secret: false,
                    ..
                }
            ));
            ui.send(&Message::Response {
                id: 2,
                value: Secret::new(b"second".to_vec()).unwrap(),
            })
            .unwrap();
        });

        let texts = [c"Information", c"Secret", c"Warning", c"Visible"];
        let styles = [
            PAM_TEXT_INFO,
            PAM_PROMPT_ECHO_OFF,
            PAM_ERROR_MSG,
            PAM_PROMPT_ECHO_ON,
        ];
        let messages = std::array::from_fn::<_, 4, _>(|index| pam_message {
            msg_style: styles[index],
            msg: texts[index].as_ptr(),
        });
        let mut pointers = messages.iter().map(std::ptr::from_ref).collect::<Vec<_>>();
        let mut responses = std::ptr::null_mut();
        let status = unsafe {
            pam_conversation(
                pointers.len() as libc::c_int,
                pointers.as_mut_ptr(),
                &mut responses,
                std::ptr::from_mut(&mut conversation).cast(),
            )
        };
        assert_eq!(status, PAM_SUCCESS);
        assert!(!responses.is_null());
        let responses = unsafe { std::slice::from_raw_parts_mut(responses, 4) };
        assert!(responses[0].resp.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(responses[1].resp) }.to_bytes(),
            b"first"
        );
        assert!(responses[2].resp.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(responses[3].resp) }.to_bytes(),
            b"second"
        );
        unsafe {
            clear_responses(responses);
            libc::free(responses.as_mut_ptr().cast());
        }
        responder.join().unwrap();
    }

    #[test]
    fn malformed_callback_input_nulls_the_output() {
        let mut responses = std::ptr::dangling_mut::<pam_response>();
        let status = unsafe {
            pam_conversation(
                0,
                std::ptr::null_mut(),
                &mut responses,
                std::ptr::null_mut(),
            )
        };

        assert_eq!(status, PAM_CONV_ERR);
        assert!(responses.is_null());
    }

    #[test]
    fn unsupported_message_style_rejects_the_whole_array() {
        let (worker, _ui) = UnixStream::pair().unwrap();
        let mut worker = Connection::new(worker);
        let mut conversation = PamConversation {
            connection: &mut worker,
            next_id: 1,
            cancelled: false,
        };
        let message = pam_message {
            msg_style: 99,
            msg: c"unsupported".as_ptr(),
        };
        let mut messages = [std::ptr::from_ref(&message)];
        let mut responses = std::ptr::dangling_mut::<pam_response>();
        let status = unsafe {
            pam_conversation(
                1,
                messages.as_mut_ptr(),
                &mut responses,
                std::ptr::from_mut(&mut conversation).cast(),
            )
        };

        assert_eq!(status, PAM_CONV_ERR);
        assert!(responses.is_null());
    }
}
