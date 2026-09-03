const MAX_TEXT_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attempt(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Response {
    Prompt { secret: bool, message: String },
    Notice { error: bool, message: String },
    Success,
    Failure(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    Prompt { secret: bool, message: String },
    Acknowledge { error: bool, message: String },
    Authenticated,
    Failed(String),
}

impl Attempt {
    pub(crate) fn initial() -> Self {
        Self(1)
    }

    pub(crate) fn advance(&mut self) -> Self {
        self.0 = self.0.wrapping_add(1);
        *self
    }
}

pub(crate) fn transition(response: Response) -> Effect {
    match response {
        Response::Prompt { secret, message } => Effect::Prompt {
            secret,
            message: clean_prompt(&message),
        },
        Response::Notice { error, message } => Effect::Acknowledge {
            error,
            message: bounded_text(&message),
        },
        Response::Success => Effect::Authenticated,
        Response::Failure(message) => Effect::Failed(bounded_text(&message)),
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
    fn maps_every_conversation_response_without_backend_effects() {
        let cases = [
            (
                Response::Prompt {
                    secret: false,
                    message: "Username: ".into(),
                },
                Effect::Prompt {
                    secret: false,
                    message: "Username".into(),
                },
            ),
            (
                Response::Prompt {
                    secret: true,
                    message: "Password: ".into(),
                },
                Effect::Prompt {
                    secret: true,
                    message: "Password".into(),
                },
            ),
            (
                Response::Notice {
                    error: false,
                    message: "Touch the security key".into(),
                },
                Effect::Acknowledge {
                    error: false,
                    message: "Touch the security key".into(),
                },
            ),
            (
                Response::Notice {
                    error: true,
                    message: "Try again".into(),
                },
                Effect::Acknowledge {
                    error: true,
                    message: "Try again".into(),
                },
            ),
            (Response::Success, Effect::Authenticated),
            (
                Response::Failure("Authentication failed".into()),
                Effect::Failed("Authentication failed".into()),
            ),
        ];

        for (response, expected) in cases {
            assert_eq!(transition(response), expected);
        }
    }

    #[test]
    fn normalizes_empty_prompts() {
        assert_eq!(clean_prompt(""), "Password");
        assert_eq!(clean_prompt(" : "), "Password");
    }

    #[test]
    fn bounds_pathological_conversation_text() {
        let bounded = bounded_text(&"界".repeat(MAX_TEXT_CHARS + 100));
        assert_eq!(bounded.chars().count(), MAX_TEXT_CHARS);
        assert!(bounded.ends_with('…'));
        assert_eq!(
            clean_prompt(&format!("{}:", "x".repeat(600))),
            bounded_text(&"x".repeat(600))
        );
    }
}
