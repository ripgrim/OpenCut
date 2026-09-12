use gpui::{Context, Window, div, prelude::*};

use crate::theme::ActiveTheme;

pub(crate) struct Timeline;

impl Render for Timeline {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = window.theme().colors;

        div()
            .flex()
            .h_1_3()
            .items_center()
            .justify_center()
            .bg(colors.card)
            .text_color(colors.card_foreground)
            .child("Timeline")
    }
}
