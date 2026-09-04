use zeroize::Zeroize;

const MAX_TEXT_CHARS: usize = 512;
const MAX_INPUT_BYTES: usize = MAX_TEXT_CHARS * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attempt(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Waiting,
    Submitting,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Response {
    Prompt { secret: bool, message: String },
    Notice { error: bool, message: String },
    Success,
    Failure(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    Prompt,
    Notice,
    Authenticated,
    Failed,
}

pub(crate) struct Conversation {
    input: String,
    prompt: String,
    message: Option<String>,
    message_is_error: bool,
    secret: bool,
    status: Status,
    attempt: Attempt,
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Conversation")
            .field("input", &"[REDACTED]")
            .field("prompt", &self.prompt)
            .field("message", &self.message)
            .field("message_is_error", &self.message_is_error)
            .field("secret", &self.secret)
            .field("status", &self.status)
            .field("attempt", &self.attempt)
            .finish()
    }
}

impl Conversation {
    pub(crate) fn new() -> Self {
        Self {
            input: response_buffer(),
            prompt: "Password".into(),
            message: None,
            message_is_error: false,
            secret: true,
            status: Status::Submitting,
            attempt: Attempt(1),
        }
    }

    pub(crate) fn begin_attempt(&mut self) -> Attempt {
        self.attempt = self.attempt.next();
        self.clear_response();
        self.clear_notice();
        self.status = Status::Submitting;
        self.attempt
    }

    pub(crate) fn invalidate_attempt(&mut self) {
        self.attempt = self.attempt.next();
        self.clear_response();
    }

    pub(crate) fn accepts(&self, attempt: Attempt) -> bool {
        attempt == self.attempt
    }

    pub(crate) fn attempt(&self) -> Attempt {
        self.attempt
    }

    pub(crate) fn input(&self) -> &str {
        &self.input
    }

    pub(crate) fn prompt(&self) -> &str {
        &self.prompt
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub(crate) fn notice_is_error(&self) -> bool {
        self.message_is_error
    }

    pub(crate) fn is_secret(&self) -> bool {
        self.secret
    }

    pub(crate) fn status(&self) -> Status {
        self.status
    }

    pub(crate) fn for_preview(
        input: String,
        prompt: String,
        message: Option<String>,
        message_is_error: bool,
        secret: bool,
        status: Status,
    ) -> Self {
        let mut bounded_input = response_buffer();
        append_bounded(&mut bounded_input, &input);
        Self {
            input: bounded_input,
            prompt: clean_prompt(&prompt),
            message: message.map(|message| bounded_text(&message)),
            message_is_error,
            secret,
            status,
            attempt: Attempt(1),
        }
    }

    pub(crate) fn update_input(&mut self, value: &str) -> bool {
        if self.status != Status::Waiting {
            return false;
        }
        self.clear_response();
        append_bounded(&mut self.input, value);
        true
    }

    pub(crate) fn push_input(&mut self, value: &str) -> bool {
        if self.status != Status::Waiting {
            return false;
        }
        let previous = self.input.len();
        append_bounded(&mut self.input, value);
        self.input.len() != previous
    }

    pub(crate) fn pop_input(&mut self) -> bool {
        if self.status != Status::Waiting {
            return false;
        }
        let Some((new_length, _)) = self.input.char_indices().next_back() else {
            return false;
        };
        // SAFETY: the selected suffix starts on a character boundary and is
        // zeroized before truncation makes it inaccessible.
        unsafe { self.input.as_bytes_mut()[new_length..].zeroize() };
        self.input.truncate(new_length);
        true
    }

    pub(crate) fn submit(&mut self) -> Option<(Attempt, String)> {
        if self.status != Status::Waiting {
            return None;
        }
        self.status = Status::Submitting;
        let response = std::mem::replace(&mut self.input, response_buffer());
        Some((self.attempt, response))
    }

    pub(crate) fn receive(&mut self, attempt: Attempt, response: Response) -> Option<Effect> {
        if !self.accepts(attempt) {
            return None;
        }

        Some(match response {
            Response::Prompt { secret, message } => {
                self.clear_response();
                self.prompt = clean_prompt(&message);
                self.secret = secret;
                self.status = Status::Waiting;
                Effect::Prompt
            }
            Response::Notice { error, message } => {
                self.set_notice(message, error);
                Effect::Notice
            }
            Response::Success => {
                self.clear_response();
                self.clear_notice();
                self.status = Status::Succeeded;
                Effect::Authenticated
            }
            Response::Failure(message) => {
                self.clear_response();
                self.prompt = "Password".into();
                self.secret = true;
                self.set_notice(message, true);
                self.status = Status::Failed;
                Effect::Failed
            }
        })
    }

    pub(crate) fn fail(&mut self, message: String) {
        let attempt = self.attempt;
        let _ = self.receive(attempt, Response::Failure(message));
    }

    pub(crate) fn set_notice(&mut self, message: String, error: bool) {
        self.message = Some(bounded_text(&message));
        self.message_is_error = error;
    }

    pub(crate) fn clear_notice(&mut self) {
        self.message = None;
        self.message_is_error = false;
    }

    pub(crate) fn clear_response(&mut self) {
        self.input.zeroize();
        self.input.clear();
        if self.input.capacity() < MAX_INPUT_BYTES {
            self.input
                .reserve_exact(MAX_INPUT_BYTES - self.input.capacity());
        }
    }
}

impl Drop for Conversation {
    fn drop(&mut self) {
        self.input.zeroize();
    }
}

impl Attempt {
    fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

fn response_buffer() -> String {
    String::with_capacity(MAX_INPUT_BYTES)
}

fn append_bounded(target: &mut String, value: &str) {
    let mut characters = target.chars().count();
    for character in value.chars() {
        if characters == MAX_TEXT_CHARS || target.len() + character.len_utf8() > MAX_INPUT_BYTES {
            break;
        }
        target.push(character);
        characters += 1;
    }
}

pub(crate) fn clean_prompt(prompt: &str) -> String {
    let prompt = prompt.trim().trim_end_matches(':').trim();
    bounded_text(if prompt.is_empty() {
        "Password"
    } else {
        prompt
    })
}

pub(crate) fn bounded_text(value: &str) -> String {
    let mut characters = value.chars();
    let mut bounded = characters.by_ref().take(MAX_TEXT_CHARS).collect::<String>();
    if characters.next().is_some() {
        bounded.pop();
        bounded.push('…');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_every_conversation_response_without_backend_actions() {
        let mut conversation = Conversation::new();
        let attempt = conversation.attempt;

        assert_eq!(
            conversation.receive(
                attempt,
                Response::Prompt {
                    secret: false,
                    message: "Username: ".into(),
                }
            ),
            Some(Effect::Prompt)
        );
        assert_eq!(conversation.status, Status::Waiting);
        assert_eq!(conversation.prompt, "Username");
        assert!(!conversation.secret);

        assert_eq!(
            conversation.receive(
                attempt,
                Response::Notice {
                    error: false,
                    message: "Touch the security key".into(),
                }
            ),
            Some(Effect::Notice)
        );
        assert_eq!(
            conversation.message.as_deref(),
            Some("Touch the security key")
        );
        assert!(!conversation.message_is_error);

        assert_eq!(
            conversation.receive(
                attempt,
                Response::Notice {
                    error: true,
                    message: "Try again".into(),
                }
            ),
            Some(Effect::Notice)
        );
        assert_eq!(conversation.message.as_deref(), Some("Try again"));
        assert!(conversation.message_is_error);

        assert_eq!(
            conversation.receive(attempt, Response::Success),
            Some(Effect::Authenticated)
        );
        assert_eq!(conversation.status, Status::Succeeded);
        assert!(conversation.message.is_none());
    }

    #[test]
    fn prompt_attempt_and_terminal_transitions_clear_responses() {
        let mut conversation = Conversation::new();
        let first = conversation.attempt;
        conversation.input = "first secret".into();

        let _ = conversation.receive(
            first,
            Response::Prompt {
                secret: true,
                message: "Password:".into(),
            },
        );
        assert!(conversation.input.is_empty());

        conversation.input = "second secret".into();
        let second = conversation.begin_attempt();
        assert_ne!(second, first);
        assert!(conversation.input.is_empty());

        let _ = conversation.receive(
            second,
            Response::Prompt {
                secret: false,
                message: "Verification code:".into(),
            },
        );
        conversation.input = "third secret".into();
        assert_eq!(
            conversation.receive(
                second,
                Response::Failure("Rejected ".repeat(MAX_TEXT_CHARS))
            ),
            Some(Effect::Failed)
        );
        assert!(conversation.input.is_empty());
        assert_eq!(conversation.prompt, "Password");
        assert!(conversation.secret);
        assert_eq!(conversation.status, Status::Failed);
        assert!(conversation.message_is_error);
        assert_eq!(
            conversation.message.as_deref().unwrap().chars().count(),
            MAX_TEXT_CHARS
        );

        conversation.input = "fourth secret".into();
        let _ = conversation.receive(second, Response::Success);
        assert!(conversation.input.is_empty());
        assert_eq!(conversation.status, Status::Succeeded);

        conversation.input = "fifth secret".into();
        conversation.invalidate_attempt();
        assert!(conversation.input.is_empty());
        assert!(!conversation.accepts(second));
    }

    #[test]
    fn stale_results_cannot_mutate_conversation_state() {
        let mut conversation = Conversation::new();
        let stale = conversation.attempt;
        let active = conversation.begin_attempt();
        conversation.input = "active response".into();

        assert_eq!(
            conversation.receive(
                stale,
                Response::Prompt {
                    secret: false,
                    message: "Stale prompt".into(),
                }
            ),
            None
        );
        assert_eq!(conversation.attempt, active);
        assert_eq!(conversation.input, "active response");
        assert_eq!(conversation.prompt, "Password");
        assert_eq!(conversation.status, Status::Submitting);
    }

    #[test]
    fn submission_moves_response_out_of_the_model() {
        let mut conversation = Conversation::new();
        let attempt = conversation.attempt;
        let _ = conversation.receive(
            attempt,
            Response::Prompt {
                secret: true,
                message: "Password:".into(),
            },
        );
        conversation.input = "secret".into();

        assert_eq!(conversation.submit(), Some((attempt, "secret".into())));
        assert!(conversation.input.is_empty());
        assert_eq!(conversation.status, Status::Submitting);
        assert_eq!(conversation.submit(), None);
    }

    #[test]
    fn diagnostics_redact_the_mutable_response() {
        let mut conversation = Conversation::new();
        let attempt = conversation.attempt();
        conversation.receive(
            attempt,
            Response::Prompt {
                secret: true,
                message: "Password".into(),
            },
        );
        conversation.update_input("credential-sentinel");

        let debug = format!("{conversation:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("credential-sentinel"));
    }

    #[test]
    fn responses_can_only_be_edited_while_waiting_for_a_prompt() {
        let mut conversation = Conversation::new();
        assert!(!conversation.update_input("too early"));
        assert!(conversation.input.is_empty());

        let attempt = conversation.attempt;
        let _ = conversation.receive(
            attempt,
            Response::Prompt {
                secret: true,
                message: "Password:".into(),
            },
        );
        assert!(conversation.update_input("accepted"));
        assert_eq!(conversation.input, "accepted");

        let _ = conversation.submit();
        assert!(!conversation.update_input("too late"));
        assert!(conversation.input.is_empty());
    }

    #[test]
    fn normalizes_and_bounds_conversation_text() {
        assert_eq!(clean_prompt(""), "Password");
        assert_eq!(clean_prompt(" : "), "Password");

        let bounded = bounded_text(&"界".repeat(MAX_TEXT_CHARS + 100));
        assert_eq!(bounded.chars().count(), MAX_TEXT_CHARS);
        assert!(bounded.ends_with('…'));
        assert_eq!(
            clean_prompt(&format!("{}:", "x".repeat(600))),
            bounded_text(&"x".repeat(600))
        );
    }

    #[test]
    fn response_storage_never_reallocates_while_editing_or_submitting() {
        let mut conversation = Conversation::new();
        let attempt = conversation.attempt();
        conversation.receive(
            attempt,
            Response::Prompt {
                secret: true,
                message: "Password".into(),
            },
        );
        let capacity = conversation.input.capacity();

        assert!(conversation.push_input(&"界".repeat(MAX_TEXT_CHARS)));
        assert_eq!(conversation.input.capacity(), capacity);
        assert!(conversation.input.len() <= MAX_INPUT_BYTES);
        assert!(conversation.pop_input());
        assert_eq!(conversation.input.capacity(), capacity);
        let (_, response) = conversation.submit().unwrap();
        assert_eq!(response.capacity(), capacity);
        assert_eq!(conversation.input.capacity(), capacity);
    }
}
