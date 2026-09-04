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
const INITIAL_NSS_BUFFER: usize = 1024;
const MAX_NSS_BUFFER: usize = 1024 * 1024;
const MAX_USERNAME_BYTES: usize = 256 * 4;

fn main() {
    if run().is_err() {
        std::process::exit(1);
    }
}

fn run() -> Result<(), ()> {
    let (fd, parent_pid) = parse_arguments()?;
    bind_to_parent(parent_pid)?;
    // SAFETY: this private binary is spawned with exclusive ownership of the
    // inherited descriptor, which is converted exactly once.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    set_close_on_exec(fd)?;
    close_extra_descriptors()?;
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
    await_begin(&mut connection)?;

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

fn await_begin(connection: &mut Connection) -> Result<(), ()> {
    (connection.receive().map_err(|_| ())? == Message::Begin)
        .then_some(())
        .ok_or(())
}

fn parse_arguments() -> Result<(RawFd, libc::pid_t), ()> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    if arguments.next().ok_or(())? != "--fd" {
        return Err(());
    }
    let fd = arguments
        .next()
        .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
        .filter(|fd: &RawFd| *fd == 3)
        .ok_or(())?;
    if arguments.next().ok_or(())? != "--parent-pid" {
        return Err(());
    }
    let parent_pid = arguments
        .next()
        .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
        .filter(|pid: &libc::pid_t| *pid > 1)
        .ok_or(())?;
    if arguments.next().is_some() {
        return Err(());
    }
    Ok((fd, parent_pid))
}

fn bind_to_parent(parent_pid: libc::pid_t) -> Result<(), ()> {
    // SAFETY: PR_SET_PDEATHSIG changes only this process. Checking getppid
    // afterward closes the race where the parent exits before prctl.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0
        || unsafe { libc::getppid() } != parent_pid
    {
        return Err(());
    }
    Ok(())
}

fn close_extra_descriptors() -> Result<(), ()> {
    // The protocol socket is descriptor 3. No PAM module should observe any
    // other descriptor inherited from the locker, especially its readiness
    // pipe. Fail closed on kernels too old to provide close_range.
    // SAFETY: close_range closes only descriptors owned by this process.
    let status = unsafe { libc::syscall(libc::SYS_close_range, 4_u32, u32::MAX, 0_u32) };
    (status == 0).then_some(()).ok_or(())
}

fn set_close_on_exec(fd: RawFd) -> Result<(), ()> {
    // Keep the private conversation out of helpers exec'd by PAM modules.
    // SAFETY: fd is the live protocol descriptor adopted above.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
        return Err(());
    }
    Ok(())
}

struct PamConversation<'a> {
    connection: &'a mut Connection,
    next_id: u64,
    cancelled: bool,
}

impl PamConversation<'_> {
    fn prompt(&mut self, text: String, secret: bool) -> Result<Secret, ()> {
        if self.next_id > MAX_MESSAGES {
            return Err(());
        }
        let id = self.next_id;
        self.next_id += 1;
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

    fn notice(&mut self, text: String, error: bool) {
        let message = if error {
            Message::Error(text)
        } else {
            Message::Info(text)
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
        // SAFETY: PAM message text is readable and NUL-terminated. The helper
        // scans and copies no more than the protocol message bound.
        let Some(text) = (unsafe { bounded_message((**message).msg) }) else {
            return PAM_CONV_ERR;
        };
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
                std::slice::from_raw_parts_mut(response.resp.cast::<u8>(), length).zeroize();
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
    username_uid(&unsafe { bounded_username(username) }?)
}

unsafe fn bounded_username(username: *const libc::c_char) -> Option<Vec<u8>> {
    // Scan one byte beyond the accepted bound so an unterminated or oversized
    // PAM_USER is rejected without unbounded preprocessing.
    let length = unsafe { libc::strnlen(username, MAX_USERNAME_BYTES + 1) };
    if length > MAX_USERNAME_BYTES {
        return None;
    }
    // SAFETY: strnlen found the terminator within the readable PAM string.
    Some(unsafe { std::slice::from_raw_parts(username.cast::<u8>(), length) }.to_vec())
}

unsafe fn bounded_message(message: *const libc::c_char) -> Option<String> {
    // Scan one byte beyond the accepted bound so an unterminated or oversized
    // PAM message fails the whole conversation instead of being truncated.
    let length = unsafe { libc::strnlen(message, MAX_MESSAGE_BYTES + 1) };
    if length > MAX_MESSAGE_BYTES {
        return None;
    }
    // SAFETY: strnlen established that these bytes precede either the bound or
    // the first NUL in the valid PAM message.
    let bytes = unsafe { std::slice::from_raw_parts(message.cast::<u8>(), length) };
    let message = String::from_utf8_lossy(bytes);
    let mut result = message.into_owned();
    while result.len() > MAX_MESSAGE_BYTES {
        result.pop();
    }
    Some(result)
}

fn username_for_uid(uid: u32) -> Option<String> {
    let mut size = INITIAL_NSS_BUFFER;
    loop {
        let mut buffer = vec![0_u8; size];
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
        if let Some(next) = next_nss_buffer(status, size) {
            size = next;
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        // SAFETY: successful NSS lookup initialized the entry and its
        // NUL-terminated name points into the live buffer.
        return unsafe { CStr::from_ptr(entry.assume_init().pw_name) }
            .to_str()
            .ok()
            .map(str::to_owned);
    }
}

fn username_uid(username: &[u8]) -> Option<u32> {
    let username = CString::new(username).ok()?;
    let mut size = INITIAL_NSS_BUFFER;
    loop {
        let mut buffer = vec![0_u8; size];
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
        if let Some(next) = next_nss_buffer(status, size) {
            size = next;
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        // SAFETY: successful NSS lookup initialized the entry.
        return Some(unsafe { entry.assume_init() }.pw_uid);
    }
}

fn next_nss_buffer(status: libc::c_int, size: usize) -> Option<usize> {
    (status == libc::ERANGE && size < MAX_NSS_BUFFER).then(|| (size * 2).min(MAX_NSS_BUFFER))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

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
            conversation.prompt("Login".into(), false).unwrap().expose(),
            b"alice"
        );
        assert_eq!(
            conversation
                .prompt("Password".into(), true)
                .unwrap()
                .expose(),
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

        assert!(conversation.prompt("Password".into(), true).is_err());
        assert!(conversation.cancelled);
        responder.join().unwrap();
    }

    #[test]
    fn bounds_lossy_pam_messages_without_truncating_oversized_input() {
        let message = CString::new("界".repeat(MAX_MESSAGE_BYTES / 3)).unwrap();
        let bounded = unsafe { bounded_message(message.as_ptr()) }.unwrap();
        assert!(bounded.len() <= MAX_MESSAGE_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));

        let accepted = CString::new(vec![b'a'; MAX_MESSAGE_BYTES]).unwrap();
        assert_eq!(
            unsafe { bounded_message(accepted.as_ptr()) }.unwrap().len(),
            MAX_MESSAGE_BYTES
        );
        let rejected = CString::new(vec![b'a'; MAX_MESSAGE_BYTES + 1]).unwrap();
        assert!(unsafe { bounded_message(rejected.as_ptr()) }.is_none());
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

    #[test]
    fn authentication_requires_an_explicit_begin_message() {
        for (message, expected) in [(Message::Begin, true), (Message::Retry, false)] {
            let (worker, ui) = UnixStream::pair().unwrap();
            let mut worker = Connection::new(worker);
            let mut ui = Connection::new(ui);
            ui.send(&message).unwrap();

            assert_eq!(await_begin(&mut worker).is_ok(), expected);
        }
    }

    #[test]
    fn bounds_post_authentication_pam_username() {
        let accepted = CString::new(vec![b'a'; MAX_USERNAME_BYTES]).unwrap();
        let rejected = CString::new(vec![b'a'; MAX_USERNAME_BYTES + 1]).unwrap();

        assert_eq!(
            unsafe { bounded_username(accepted.as_ptr()) }
                .unwrap()
                .len(),
            MAX_USERNAME_BYTES
        );
        assert!(unsafe { bounded_username(rejected.as_ptr()) }.is_none());
    }

    #[test]
    fn nss_buffer_growth_stops_at_the_fixed_cap() {
        assert_eq!(
            next_nss_buffer(libc::ERANGE, INITIAL_NSS_BUFFER),
            Some(INITIAL_NSS_BUFFER * 2)
        );
        assert_eq!(
            next_nss_buffer(libc::ERANGE, MAX_NSS_BUFFER / 2 + 1),
            Some(MAX_NSS_BUFFER)
        );
        assert_eq!(next_nss_buffer(libc::ERANGE, MAX_NSS_BUFFER), None);
        assert_eq!(next_nss_buffer(0, INITIAL_NSS_BUFFER), None);
    }

    #[test]
    fn protocol_descriptor_is_closed_across_exec() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        set_close_on_exec(stream.as_raw_fd()).unwrap();

        let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
}
