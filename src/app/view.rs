use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, responsive, row, scrollable, stack, text, text_input, Space,
};
use iced::{Alignment, Color, Element, Fill, Length};

use crate::accounts::Account;
use crate::power::Action as PowerAction;
use crate::{background, theme};

use super::auth_flow::Phase;
use super::{App, Message, PowerState};

const ACCOUNT_TILE_WIDTH: f32 = 148.0;
const ACCOUNT_GRID_GAP: f32 = 18.0;
const MAX_ACCOUNT_COLUMNS: usize = 4;
const AUTH_ACTION_WIDTH: f32 = 82.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountSelectorState {
    Interactive,
    Disabled,
    Hidden,
}

impl App {
    pub(crate) fn view(&self) -> Element<'_, Message> {
        let background = background::Background::new(self.background_elapsed()).view();
        let clock = column![
            text(self.now.format("%-I:%M").to_string())
                .size(80)
                .color(Color::WHITE),
            text(self.now.format("%A, %B %-d").to_string())
                .size(22)
                .color(Color::from_rgba8(255, 255, 255, 0.85)),
        ]
        .align_x(Alignment::Center)
        .spacing(0);

        let selector_state = account_selector_state(
            self.phase,
            self.can_select_account(),
            !self.username.is_empty(),
        );
        let identity: Element<'_, Message> = match selector_state {
            AccountSelectorState::Interactive => self.account_selection(true),
            AccountSelectorState::Disabled => self.account_selection(false),
            AccountSelectorState::Hidden => self.authentication(),
        };

        let center = container(
            column![clock, Space::new(Fill, Fill), identity]
                .width(Fill)
                .height(Fill)
                .align_x(Alignment::Center),
        )
        .width(Fill)
        .height(Fill)
        .padding([42, 24]);

        let power_interactive = self.can_request_power();
        let power_buttons = row![
            power_button(PowerAction::Suspend, power_interactive),
            power_button(PowerAction::Reboot, power_interactive),
            power_button(PowerAction::PowerOff, power_interactive),
        ]
        .spacing(10);
        let utilities = container(power_buttons)
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::Start)
            .padding([28, 30]);

        let session = container(self.session_selector())
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::Start)
            .align_y(Alignment::End)
            .padding([28, 30]);

        let main_content = stack![center, utilities, session];
        let content: Element<'_, Message> = match self.power_state {
            PowerState::Confirming(action) => {
                let dialog_interactive = self.power_dialog_interactive();
                let confirmation = container(
                    column![
                        text(format!("{} this computer?", action.label())).size(24),
                        row![
                            button("Cancel")
                                .on_press_maybe(dialog_interactive.then_some(Message::CancelPower)),
                            button(action.label()).on_press_maybe(
                                dialog_interactive.then_some(Message::ConfirmPower(action))
                            ),
                        ]
                        .spacing(12),
                    ]
                    .align_x(Alignment::Center)
                    .spacing(22),
                )
                .padding(30)
                .style(theme::panel);
                modal(main_content, confirmation)
            }
            PowerState::Executing(action) => {
                let progress = container(
                    text(format!("Requesting {}…", action.label().to_lowercase())).size(22),
                )
                .padding(30)
                .style(theme::panel);
                modal(main_content, progress)
            }
            PowerState::Idle => main_content.into(),
        };

        stack![background, content].into()
    }

    fn account_selection(&self, interactive: bool) -> Element<'_, Message> {
        let status = self.status_for(
            self.message
                .as_deref()
                .filter(|message| *message != "Select a user"),
        );
        let retry: Element<'_, Message> = if self.phase == Phase::UserSelectionCancellationFailed
            || (self.phase == Phase::Failed && self.username.is_empty())
        {
            let (label, message) = if self.phase == Phase::UserSelectionCancellationFailed {
                (
                    "Retry changing user",
                    Message::RetryUserSelectionCancellation,
                )
            } else {
                ("Retry account discovery", Message::Retry)
            };
            button(text(label).size(15))
                .on_press_maybe(
                    (self.closing.is_none() && self.power_state == PowerState::Idle)
                        .then_some(message),
                )
                .padding([10, 18])
                .style(theme::translucent_button)
                .into()
        } else {
            Space::new(Length::Shrink, Length::Fixed(0.0)).into()
        };

        container(
            column![
                text("Select a user").size(28).color(Color::WHITE),
                account_grid(&self.accounts, interactive),
                status,
                retry,
            ]
            .width(Fill)
            .align_x(Alignment::Center)
            .spacing(14),
        )
        .width(Fill)
        .max_width(780)
        .into()
    }

    fn authentication(&self) -> Element<'_, Message> {
        let interactive = self.closing.is_none() && self.power_state == PowerState::Idle;
        let selected_account = self
            .accounts
            .iter()
            .find(|account| account.username == self.username);
        let username = selected_account
            .map(|account| format!("@{}", account.username))
            .unwrap_or_else(|| format!("@{}", self.username));
        let prompt = text(&self.prompt)
            .size(15)
            .width(Fill)
            .align_x(Alignment::Center)
            .wrapping(Wrapping::WordOrGlyph);
        let input = text_input("", &self.input)
            .id(self.input_id.clone())
            .on_input_maybe(
                (interactive && self.phase == Phase::WaitingForInput)
                    .then_some(Message::InputChanged),
            )
            .on_submit_maybe(
                (interactive && self.phase == Phase::WaitingForInput).then_some(Message::Submit),
            )
            .secure(self.secret)
            .padding([12, 18])
            .size(18)
            .width(Fill)
            .style(theme::input);
        let submit = if self.phase == Phase::Failed {
            button(text("Retry").size(16))
                .on_press_maybe(interactive.then_some(Message::Retry))
                .padding([12, 18])
                .width(Length::Fixed(AUTH_ACTION_WIDTH))
                .style(theme::primary_button)
        } else {
            button(text("Log In").size(16))
                .on_press_maybe(
                    (interactive && self.phase == Phase::WaitingForInput)
                        .then_some(Message::Submit),
                )
                .padding([12, 18])
                .width(Length::Fixed(AUTH_ACTION_WIDTH))
                .style(theme::primary_button)
        };
        let change_user: Element<'_, Message> = if self.can_change_user() {
            button(text("Change User").size(14))
                .on_press(Message::ChangeUser)
                .padding([8, 14])
                .style(theme::translucent_button)
                .into()
        } else {
            Space::new(Length::Shrink, Length::Fixed(0.0)).into()
        };

        container(
            column![
                avatar(&self.display_name, 100.0, 38),
                column![
                    text(&self.display_name).size(28).color(Color::WHITE),
                    text(username)
                        .size(14)
                        .color(Color::from_rgba8(255, 255, 255, 0.68)),
                ]
                .align_x(Alignment::Center)
                .spacing(2),
                column![
                    prompt,
                    row![
                        Space::new(Length::Fixed(AUTH_ACTION_WIDTH), Length::Shrink),
                        input,
                        submit
                    ]
                    .spacing(8)
                    .width(Fill),
                    self.status_for(self.message.as_deref())
                ]
                .width(Fill)
                .spacing(8),
                change_user,
            ]
            .width(Fill)
            .align_x(Alignment::Center)
            .spacing(14),
        )
        .width(Fill)
        .max_width(540)
        .into()
    }

    fn status_for<'a>(&'a self, message: Option<&'a str>) -> Element<'a, Message> {
        let status = message.unwrap_or(" ");
        let color = if self.message_is_error {
            Color::from_rgb8(255, 171, 171)
        } else {
            Color::from_rgba8(255, 255, 255, 0.78)
        };
        container(
            text(status)
                .size(14)
                .color(color)
                .width(Fill)
                .align_x(Alignment::Center)
                .wrapping(Wrapping::WordOrGlyph),
        )
        .width(Fill)
        .height(Length::Fixed(40.0))
        .align_x(Alignment::Center)
        .into()
    }

    fn session_selector(&self) -> Element<'_, Message> {
        let selector: Element<'_, Message> = if self.can_select_session() {
            iced::widget::pick_list(
                self.sessions.as_slice(),
                self.selected_session.as_ref(),
                Message::SelectSession,
            )
            .padding([9, 14])
            .style(theme::selector)
            .menu_style(theme::selector_menu)
            .width(Length::Fixed(210.0))
            .into()
        } else {
            container(
                text(
                    self.selected_session
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "No session available".into()),
                )
                .size(15),
            )
            .width(Length::Fixed(210.0))
            .padding([9, 14])
            .style(theme::selection)
            .into()
        };
        column![
            text("Session")
                .size(13)
                .color(Color::from_rgba8(255, 255, 255, 0.72)),
            selector,
        ]
        .spacing(6)
        .into()
    }
}

fn account_grid<'a>(accounts: &'a [Account], interactive: bool) -> Element<'a, Message> {
    container(responsive(move |size| {
        let columns = account_grid_columns(size.width);
        let rows = accounts
            .chunks(columns)
            .map(|accounts| {
                row(accounts
                    .iter()
                    .map(|account| account_tile(account, interactive)))
                .spacing(ACCOUNT_GRID_GAP)
                .align_y(Alignment::Start)
            })
            .map(Element::from)
            .collect::<Vec<_>>();

        scrollable(
            column(rows)
                .width(Fill)
                .align_x(Alignment::Center)
                .spacing(ACCOUNT_GRID_GAP),
        )
        .width(Fill)
        .height(Fill)
        .into()
    }))
    .width(Fill)
    .height(Length::Fixed(292.0))
    .into()
}

fn account_tile<'a>(account: &'a Account, interactive: bool) -> Element<'a, Message> {
    button(
        column![
            avatar(&account.display_name, 76.0, 28),
            text(&account.display_name)
                .size(16)
                .width(Fill)
                .align_x(Alignment::Center)
                .wrapping(Wrapping::WordOrGlyph),
            text(format!("@{}", account.username))
                .size(13)
                .color(Color::from_rgba8(255, 255, 255, 0.68))
                .width(Fill)
                .align_x(Alignment::Center)
                .wrapping(Wrapping::WordOrGlyph),
        ]
        .width(Fill)
        .align_x(Alignment::Center)
        .spacing(7),
    )
    .on_press_maybe(interactive.then(|| Message::SelectAccount(account.clone())))
    .width(Length::Fixed(ACCOUNT_TILE_WIDTH))
    .height(Length::Fixed(164.0))
    .padding([12, 10])
    .style(theme::account_tile)
    .into()
}

fn avatar<'a>(name: &str, diameter: f32, text_size: u16) -> Element<'a, Message> {
    container(text(initials(name)).size(text_size).color(Color::WHITE))
        .width(Length::Fixed(diameter))
        .height(Length::Fixed(diameter))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(move |_| theme::avatar(diameter / 2.0))
        .into()
}

fn account_grid_columns(width: f32) -> usize {
    (((width + ACCOUNT_GRID_GAP) / (ACCOUNT_TILE_WIDTH + ACCOUNT_GRID_GAP)) as usize)
        .clamp(1, MAX_ACCOUNT_COLUMNS)
}

fn account_selector_state(
    phase: Phase,
    interactive: bool,
    has_identity: bool,
) -> AccountSelectorState {
    match phase {
        Phase::SelectingUser if interactive => AccountSelectorState::Interactive,
        Phase::SelectingUser
        | Phase::CancellingForUserSelection
        | Phase::UserSelectionCancellationFailed => AccountSelectorState::Disabled,
        Phase::Failed if !has_identity => AccountSelectorState::Disabled,
        _ => AccountSelectorState::Hidden,
    }
}

fn modal<'a>(
    main_content: impl Into<Element<'a, Message>>,
    dialog: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    stack![
        main_content.into(),
        container(dialog)
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
    ]
    .into()
}

fn power_button(action: PowerAction, interactive: bool) -> Element<'static, Message> {
    button(text(action.label()).size(14))
        .on_press_maybe(interactive.then_some(Message::AskPower(action)))
        .padding([9, 15])
        .style(theme::translucent_button)
        .into()
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

    #[test]
    fn account_selector_is_disabled_until_cancellation_succeeds() {
        assert_eq!(
            account_selector_state(Phase::CancellingForUserSelection, false, false),
            AccountSelectorState::Disabled
        );
        assert_eq!(
            account_selector_state(Phase::UserSelectionCancellationFailed, false, false),
            AccountSelectorState::Disabled
        );
        assert_eq!(
            account_selector_state(Phase::SelectingUser, true, false),
            AccountSelectorState::Interactive
        );
        assert_eq!(
            account_selector_state(Phase::WaitingForInput, false, true),
            AccountSelectorState::Hidden
        );
        assert_eq!(
            account_selector_state(Phase::Failed, false, false),
            AccountSelectorState::Disabled
        );
    }

    #[test]
    fn account_grid_wraps_at_narrow_and_wide_sizes() {
        assert_eq!(account_grid_columns(140.0), 1);
        assert_eq!(account_grid_columns(320.0), 2);
        assert_eq!(account_grid_columns(520.0), 3);
        assert_eq!(account_grid_columns(900.0), MAX_ACCOUNT_COLUMNS);
    }

    #[test]
    fn creates_initials() {
        assert_eq!(initials("Darwin Wu"), "DW");
        assert_eq!(initials("Darwin"), "D");
    }
}
