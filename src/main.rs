mod auth;
mod background;
mod power;
mod sessions;
mod theme;

use std::time::{Duration, Instant};

use auth::Client;
use chrono::Local;
use clap::Parser;
use greetd_ipc::Request;
use iced::widget::{button, column, container, pick_list, row, stack, text, text_input, Space};
use iced::{time, window, Alignment, Color, Element, Fill, Length, Subscription, Task, Theme};
use power::Action as PowerAction;
use sessions::Session;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    #[arg(long, default_value = "darwin")]
    username: String,
    #[arg(long, default_value = "Darwin")]
    display_name: String,
    #[arg(long, default_value = "sway --unsupported-gpu")]
    session_command: String,
    #[arg(long)]
    windowed: bool,
}

pub fn main() -> iced::Result {
    let arguments = Arguments::parse();
    let windowed = arguments.windowed;
    iced::application("Genkan", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::Dark)
        .window(window::Settings {
            size: iced::Size::new(1280.0, 800.0),
            decorations: windowed,
            ..Default::default()
        })
        .exit_on_close_request(false)
        .antialiasing(true)
        .run_with(|| App::new(arguments))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    CreatingSession,
    WaitingForInput,
    Authenticating,
    StartingSession,
}

#[derive(Debug)]
struct App {
    username: String,
    display_name: String,
    input: String,
    prompt: String,
    message: Option<String>,
    message_is_error: bool,
    secret: bool,
    phase: Phase,
    client: Option<Client>,
    sessions: Vec<Session>,
    selected_session: Session,
    started_at: Instant,
    now: chrono::DateTime<Local>,
    confirmation: Option<PowerAction>,
    attempt: u64,
    closing: Option<window::Id>,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    InputChanged(String),
    Submit,
    AuthResult {
        attempt: u64,
        result: Result<(Option<Client>, auth::Response), String>,
    },
    SelectSession(Session),
    AskPower(PowerAction),
    CancelPower,
    ConfirmPower(PowerAction),
    PowerResult(Result<(), String>),
    CloseRequested(window::Id),
    SessionCancelled(window::Id),
}

impl App {
    fn new(arguments: Arguments) -> (Self, Task<Message>) {
        let command = shell_words::split(&arguments.session_command)
            .ok()
            .filter(|parts| !parts.is_empty())
            .unwrap_or_else(|| vec!["sway".into(), "--unsupported-gpu".into()]);
        let fallback = Session::sway(command);
        let sessions = sessions::discover(fallback.clone());
        let selected_session = sessions
            .iter()
            .find(|session| session.command == fallback.command)
            .cloned()
            .unwrap_or(fallback);

        let app = Self {
            username: arguments.username,
            display_name: arguments.display_name,
            input: String::new(),
            prompt: "Password".into(),
            message: None,
            message_is_error: false,
            secret: true,
            phase: Phase::CreatingSession,
            client: None,
            sessions,
            selected_session,
            started_at: Instant::now(),
            now: Local::now(),
            confirmation: None,
            attempt: 1,
            closing: None,
        };
        let task = begin_authentication(app.username.clone(), app.attempt, true);
        (app, task)
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            time::every(Duration::from_millis(50)).map(|_| Message::Tick),
            window::close_requests().map(Message::CloseRequested),
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                self.now = Local::now();
                Task::none()
            }
            Message::InputChanged(value) => {
                self.input = value;
                Task::none()
            }
            Message::SelectSession(session)
                if !matches!(self.phase, Phase::Authenticating | Phase::StartingSession) =>
            {
                self.selected_session = session;
                Task::none()
            }
            Message::SelectSession(_) => Task::none(),
            Message::Submit if self.phase == Phase::Idle => {
                self.message = None;
                self.phase = Phase::CreatingSession;
                let client = self.client.take();
                let attempt = self.next_attempt();
                restart_authentication(client, self.username.clone(), attempt)
            }
            Message::Submit if self.phase == Phase::WaitingForInput => {
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                let response = std::mem::take(&mut self.input);
                self.phase = Phase::Authenticating;
                exchange(
                    client,
                    Request::PostAuthMessageResponse {
                        response: Some(response),
                    },
                    self.attempt,
                )
            }
            Message::Submit => Task::none(),
            Message::AuthResult { attempt, result } => {
                if attempt != self.attempt {
                    return Task::none();
                }
                if let Some(window) = self.closing.take() {
                    let client = match result {
                        Ok((client, _)) => client,
                        Err(_) => None,
                    };
                    self.next_attempt();
                    return cancel_and_close(client, window);
                }
                match result {
                    Ok((client, response)) => {
                        if let Some(client) = client {
                            self.client = Some(client);
                        }
                        self.handle_auth_response(response)
                    }
                    Err(error) => self.fail(error),
                }
            }
            Message::AskPower(action) => {
                self.confirmation = Some(action);
                Task::none()
            }
            Message::CancelPower => {
                self.confirmation = None;
                Task::none()
            }
            Message::ConfirmPower(action) => {
                self.confirmation = None;
                self.message = Some(format!("Requesting {}…", action.label().to_lowercase()));
                self.message_is_error = false;
                Task::perform(power::execute(action), |result| {
                    Message::PowerResult(result.map_err(|error| error.to_string()))
                })
            }
            Message::PowerResult(Ok(())) => Task::none(),
            Message::PowerResult(Err(error)) => {
                self.message = Some(error);
                self.message_is_error = true;
                Task::none()
            }
            Message::CloseRequested(window) if self.client.is_some() => {
                self.next_attempt();
                cancel_and_close(self.client.take(), window)
            }
            Message::CloseRequested(window) if self.phase == Phase::CreatingSession => {
                self.closing = Some(window);
                Task::none()
            }
            Message::CloseRequested(window) => window::close(window),
            Message::SessionCancelled(window) => window::close(window),
        }
    }

    fn handle_auth_response(&mut self, response: auth::Response) -> Task<Message> {
        match response {
            auth::Response::Prompt { secret, message } => {
                self.prompt = clean_prompt(&message);
                self.secret = secret;
                self.input.clear();
                self.phase = Phase::WaitingForInput;
                Task::none()
            }
            auth::Response::Message { error, message } => {
                self.message = Some(message);
                self.message_is_error = error;
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                exchange(
                    client,
                    Request::PostAuthMessageResponse { response: None },
                    self.attempt,
                )
            }
            auth::Response::Success if self.phase == Phase::StartingSession => iced::exit(),
            auth::Response::Success => {
                let Some(client) = self.client.clone() else {
                    return self.fail("Lost connection to greetd".into());
                };
                self.phase = Phase::StartingSession;
                self.message = Some("Starting session…".into());
                let session = self.selected_session.clone();
                let attempt = self.attempt;
                Task::perform(
                    async move {
                        let environment = session.environment();
                        client
                            .exchange(Request::StartSession {
                                cmd: session.command,
                                env: environment,
                            })
                            .await
                    },
                    move |result| Message::AuthResult {
                        attempt,
                        result: result
                            .map(|response| (None, response))
                            .map_err(|error| error.to_string()),
                    },
                )
            }
            auth::Response::Error {
                authentication,
                description,
            } => {
                if authentication {
                    self.client = None;
                    self.phase = Phase::CreatingSession;
                    self.input.clear();
                    self.prompt = "Password".into();
                    self.secret = true;
                    self.message = Some("Authentication failed".into());
                    self.message_is_error = true;
                    let attempt = self.next_attempt();
                    begin_authentication(self.username.clone(), attempt, false)
                } else {
                    self.fail(description)
                }
            }
        }
    }

    fn fail(&mut self, message: String) -> Task<Message> {
        self.phase = Phase::Idle;
        self.input.clear();
        self.prompt = "Password".into();
        self.secret = true;
        self.message = Some(message);
        self.message_is_error = true;
        Task::none()
    }

    fn next_attempt(&mut self) -> u64 {
        self.attempt = self.attempt.wrapping_add(1);
        self.attempt
    }

    fn view(&self) -> Element<'_, Message> {
        let elapsed = self.started_at.elapsed().as_secs_f32();
        let background = background::Background::new(elapsed).view();
        let clock = text(self.now.format("%-I:%M").to_string())
            .size(80)
            .color(Color::WHITE);
        let date = text(self.now.format("%A, %B %-d").to_string())
            .size(22)
            .color(Color::from_rgba8(255, 255, 255, 0.85));

        let avatar = container(
            text(initials(&self.display_name))
                .size(38)
                .color(Color::WHITE),
        )
        .width(Length::Fixed(92.0))
        .height(Length::Fixed(92.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(Color::from_rgba8(
                255, 255, 255, 0.18,
            ))),
            border: iced::Border {
                color: Color::from_rgba8(255, 255, 255, 0.45),
                width: 2.0,
                radius: 46.0.into(),
            },
            ..Default::default()
        });

        let input = text_input(&self.prompt, &self.input)
            .on_input(Message::InputChanged)
            .on_submit(Message::Submit)
            .secure(self.secret)
            .padding([12, 18])
            .size(18)
            .style(theme::input);
        let submit = button(text("→").size(22))
            .on_press_maybe(
                matches!(self.phase, Phase::Idle | Phase::WaitingForInput)
                    .then_some(Message::Submit),
            )
            .padding([10, 16])
            .style(theme::translucent_button);
        let auth_row = row![input, submit].spacing(8).width(Length::Fixed(340.0));

        let status = self.message.as_deref().unwrap_or(" ");
        let status_color = if self.message_is_error {
            Color::from_rgb8(255, 151, 151)
        } else {
            Color::from_rgba8(255, 255, 255, 0.75)
        };

        let login_panel = container(
            column![
                avatar,
                text(&self.display_name).size(26).color(Color::WHITE),
                auth_row,
                text(status).size(14).color(status_color),
                pick_list(
                    self.sessions.as_slice(),
                    Some(&self.selected_session),
                    Message::SelectSession
                )
                .width(Length::Fixed(220.0)),
            ]
            .spacing(14)
            .align_x(Alignment::Center),
        )
        .padding([28, 36])
        .style(theme::panel);

        let power_buttons = row![
            power_button(PowerAction::Suspend),
            power_button(PowerAction::Reboot),
            power_button(PowerAction::PowerOff),
        ]
        .spacing(14);

        let main_content = column![
            column![clock, date].align_x(Alignment::Center).spacing(0),
            Space::new(Fill, Length::Fixed(36.0)),
            login_panel,
            Space::new(Fill, Length::Fixed(28.0)),
            power_buttons,
        ]
        .width(Fill)
        .height(Fill)
        .align_x(Alignment::Center)
        .padding([44, 20]);

        let content: Element<'_, Message> = if let Some(action) = self.confirmation {
            let confirmation = container(
                column![
                    text(format!("{} this computer?", action.label())).size(24),
                    row![
                        button("Cancel").on_press(Message::CancelPower),
                        button(action.label()).on_press(Message::ConfirmPower(action)),
                    ]
                    .spacing(12),
                ]
                .align_x(Alignment::Center)
                .spacing(22),
            )
            .padding(30)
            .style(theme::panel);
            stack![
                main_content,
                container(confirmation)
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center)
            ]
            .into()
        } else {
            main_content.into()
        };

        stack![background, content].into()
    }
}

fn begin_authentication(username: String, attempt: u64, recover: bool) -> Task<Message> {
    Task::perform(
        async move {
            if recover {
                auth::recover_and_begin(username).await
            } else {
                auth::begin(username).await
            }
        },
        move |result| Message::AuthResult {
            attempt,
            result: result
                .map(|(client, response)| (Some(client), response))
                .map_err(|error| error.to_string()),
        },
    )
}

fn restart_authentication(client: Option<Client>, username: String, attempt: u64) -> Task<Message> {
    Task::perform(
        async move {
            let needs_recovery = match client {
                Some(client) => auth::cancel(Some(client)).await.is_err(),
                None => true,
            };
            if needs_recovery {
                auth::recover_and_begin(username).await
            } else {
                auth::begin(username).await
            }
        },
        move |result| Message::AuthResult {
            attempt,
            result: result
                .map(|(client, response)| (Some(client), response))
                .map_err(|error| error.to_string()),
        },
    )
}

fn exchange(client: Client, request: Request, attempt: u64) -> Task<Message> {
    Task::perform(
        async move { client.exchange(request).await },
        move |result| Message::AuthResult {
            attempt,
            result: result
                .map(|response| (None, response))
                .map_err(|error| error.to_string()),
        },
    )
}

fn cancel_and_close(client: Option<Client>, window: window::Id) -> Task<Message> {
    Task::perform(auth::cancel(client), move |_| {
        Message::SessionCancelled(window)
    })
}

fn power_button(action: PowerAction) -> Element<'static, Message> {
    button(text(action.label()).size(14))
        .on_press(Message::AskPower(action))
        .padding([10, 18])
        .style(theme::translucent_button)
        .into()
}

fn clean_prompt(prompt: &str) -> String {
    let prompt = prompt.trim().trim_end_matches(':').trim();
    if prompt.is_empty() {
        "Password".into()
    } else {
        prompt.into()
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App {
            username: "darwin".into(),
            display_name: "Darwin".into(),
            input: "secret".into(),
            prompt: "Password".into(),
            message: Some("Keep this message".into()),
            message_is_error: false,
            secret: true,
            phase: Phase::WaitingForInput,
            client: None,
            sessions: vec![Session::sway(vec!["sway".into()])],
            selected_session: Session::sway(vec!["sway".into()]),
            started_at: Instant::now(),
            now: Local::now(),
            confirmation: None,
            attempt: 2,
            closing: None,
        }
    }

    #[test]
    fn normalizes_pam_prompts() {
        assert_eq!(clean_prompt("Password: "), "Password");
        assert_eq!(clean_prompt(""), "Password");
    }

    #[test]
    fn creates_initials() {
        assert_eq!(initials("Darwin Wu"), "DW");
        assert_eq!(initials("Darwin"), "D");
    }

    #[test]
    fn ignores_responses_from_abandoned_attempts() {
        let mut app = app();
        let _ = app.update(Message::AuthResult {
            attempt: 1,
            result: Ok((
                None,
                auth::Response::Error {
                    authentication: false,
                    description: "late failure".into(),
                },
            )),
        });

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.input, "secret");
        assert_eq!(app.message.as_deref(), Some("Keep this message"));
    }

    #[test]
    fn power_failures_preserve_authentication_state() {
        let mut app = app();
        let _ = app.update(Message::PowerResult(Err("not authorized".into())));

        assert_eq!(app.phase, Phase::WaitingForInput);
        assert_eq!(app.input, "secret");
        assert_eq!(app.message.as_deref(), Some("not authorized"));
        assert!(app.message_is_error);
    }
}
