use iced::widget::{button, column, container, pick_list, row, stack, text, text_input, Space};
use iced::{Alignment, Color, Element, Fill, Length};

use crate::power::Action as PowerAction;
use crate::{background, theme};

use super::auth_flow::Phase;
use super::{App, Message};

impl App {
    pub(crate) fn view(&self) -> Element<'_, Message> {
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

        let interactive = self.closing.is_none();
        let input = text_input(&self.prompt, &self.input)
            .on_input_maybe(interactive.then_some(Message::InputChanged))
            .on_submit_maybe(interactive.then_some(Message::Submit))
            .secure(self.secret)
            .padding([12, 18])
            .size(18)
            .style(theme::input);
        let submit = button(text("→").size(22))
            .on_press_maybe(
                (interactive && matches!(self.phase, Phase::Idle | Phase::WaitingForInput))
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
        let session_selector: Element<'_, Message> = if interactive {
            pick_list(
                self.sessions.as_slice(),
                self.selected_session.as_ref(),
                Message::SelectSession,
            )
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
                text(&self.display_name).size(26).color(Color::WHITE),
                auth_row,
                text(status).size(14).color(status_color),
                session_selector,
            ]
            .spacing(14)
            .align_x(Alignment::Center),
        )
        .padding([28, 36])
        .style(theme::panel);

        let power_buttons = row![
            power_button(PowerAction::Suspend, interactive),
            power_button(PowerAction::Reboot, interactive),
            power_button(PowerAction::PowerOff, interactive),
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
                        button("Cancel")
                            .on_press_maybe(interactive.then_some(Message::CancelPower)),
                        button(action.label())
                            .on_press_maybe(interactive.then_some(Message::ConfirmPower(action))),
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
    fn creates_initials() {
        assert_eq!(initials("Darwin Wu"), "DW");
        assert_eq!(initials("Darwin"), "D");
    }
}
