use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, pick_list, row, scrollable, stack, text, text_input, Space,
};
use iced::{Alignment, Color, Element, Fill, Length};

use crate::power::Action as PowerAction;
use crate::{background, theme};

use super::auth_flow::Phase;
use super::{App, Message, PowerState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountSelectorState {
    Interactive,
    Disabled,
    Hidden,
}

impl App {
    pub(crate) fn view(&self) -> Element<'_, Message> {
        let background = background::Background::new(self.background_elapsed()).view();
        let clock = text(self.now.format("%-I:%M").to_string())
            .size(80)
            .color(Color::WHITE);
        let date = text(self.now.format("%A, %B %-d").to_string())
            .size(22)
            .color(Color::from_rgba8(255, 255, 255, 0.85));

        let avatar_content: Element<'_, Message> = text(initials(&self.display_name))
            .size(38)
            .color(Color::WHITE)
            .into();
        let avatar = container(avatar_content)
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

        let interactive = self.closing.is_none() && self.power_state == PowerState::Idle;
        let selected_account = self
            .accounts
            .iter()
            .find(|account| account.username == self.username);
        let account_selector: Element<'_, Message> =
            match account_selector_state(self.phase, self.can_select_account()) {
                AccountSelectorState::Interactive => pick_list(
                    self.accounts.as_slice(),
                    selected_account,
                    Message::SelectAccount,
                )
                .placeholder("Select account")
                .padding([10, 16])
                .style(theme::selector)
                .menu_style(theme::selector_menu)
                .width(Length::Fixed(260.0))
                .into(),
                AccountSelectorState::Disabled => container(text("Select account"))
                    .width(Length::Fixed(260.0))
                    .padding([10, 16])
                    .style(theme::selection)
                    .into(),
                AccountSelectorState::Hidden => container(text(
                    selected_account
                        .map(ToString::to_string)
                        .unwrap_or_else(|| self.display_name.clone()),
                ))
                .width(Length::Fixed(260.0))
                .padding([10, 16])
                .style(theme::selection)
                .into(),
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
        let prompt = text(&self.prompt)
            .size(15)
            .width(Fill)
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
        let submit = if self.phase == Phase::UserSelectionCancellationFailed {
            button(text("Retry").size(16))
                .on_press_maybe(interactive.then_some(Message::RetryUserSelectionCancellation))
                .padding([12, 16])
                .style(theme::translucent_button)
        } else if self.phase == Phase::Failed {
            button(text("Retry").size(16))
                .on_press_maybe(interactive.then_some(Message::Retry))
                .padding([12, 16])
                .style(theme::translucent_button)
        } else {
            button(text("→").size(22))
                .on_press_maybe(
                    (interactive && self.phase == Phase::WaitingForInput)
                        .then_some(Message::Submit),
                )
                .padding([10, 16])
                .style(theme::translucent_button)
        };
        let auth_row = row![input, submit].spacing(8).width(Fill);

        let status = self.message.as_deref().unwrap_or(" ");
        let status_color = if self.message_is_error {
            Color::from_rgb8(255, 151, 151)
        } else {
            Color::from_rgba8(255, 255, 255, 0.75)
        };
        let status = scrollable(
            text(status)
                .size(14)
                .color(status_color)
                .width(Fill)
                .wrapping(Wrapping::WordOrGlyph),
        )
        .width(Fill)
        .height(Length::Fixed(52.0));
        let session_selector: Element<'_, Message> = if self.can_select_session() {
            pick_list(
                self.sessions.as_slice(),
                self.selected_session.as_ref(),
                Message::SelectSession,
            )
            .padding([10, 16])
            .style(theme::selector)
            .menu_style(theme::selector_menu)
            .width(Length::Fixed(220.0))
            .into()
        } else {
            container(
                text(
                    self.selected_session
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "No session available".into()),
                )
                .size(16),
            )
            .width(Length::Fixed(220.0))
            .height(Length::Fixed(32.0))
            .padding([0, 12])
            .align_y(Alignment::Center)
            .into()
        };

        let login_panel = container(
            column![
                avatar,
                account_selector,
                change_user,
                prompt,
                auth_row,
                status,
                session_selector,
            ]
            .spacing(14)
            .width(Fill)
            .align_x(Alignment::Center),
        )
        .width(Fill)
        .max_width(420)
        .padding([28, 36])
        .style(theme::panel);

        let power_interactive = self.can_request_power();
        let power_buttons = row![
            power_button(PowerAction::Suspend, power_interactive),
            power_button(PowerAction::Reboot, power_interactive),
            power_button(PowerAction::PowerOff, power_interactive),
        ]
        .spacing(14);

        let main_content = scrollable(
            column![
                column![clock, date].align_x(Alignment::Center).spacing(0),
                Space::new(Fill, Length::Fixed(36.0)),
                login_panel,
                Space::new(Fill, Length::Fixed(28.0)),
                power_buttons,
            ]
            .width(Fill)
            .align_x(Alignment::Center)
            .padding([44, 20]),
        )
        .width(Fill)
        .height(Fill);

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
}

fn account_selector_state(phase: Phase, interactive: bool) -> AccountSelectorState {
    match phase {
        Phase::SelectingUser if interactive => AccountSelectorState::Interactive,
        Phase::SelectingUser
        | Phase::CancellingForUserSelection
        | Phase::UserSelectionCancellationFailed => AccountSelectorState::Disabled,
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
        .padding([10, 18])
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
            account_selector_state(Phase::CancellingForUserSelection, false),
            AccountSelectorState::Disabled
        );
        assert_eq!(
            account_selector_state(Phase::UserSelectionCancellationFailed, false),
            AccountSelectorState::Disabled
        );
        assert_eq!(
            account_selector_state(Phase::SelectingUser, true),
            AccountSelectorState::Interactive
        );
        assert_eq!(
            account_selector_state(Phase::WaitingForInput, false),
            AccountSelectorState::Hidden
        );
    }

    #[test]
    fn creates_initials() {
        assert_eq!(initials("Darwin Wu"), "DW");
        assert_eq!(initials("Darwin"), "D");
    }
}
