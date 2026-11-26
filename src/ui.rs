use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme, Selectable, Sizable, Theme,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    v_flex,
};
use std::rc::Rc;

pub(crate) fn init_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let dark = &mut Rc::make_mut(&mut theme.dark_theme).colors;
    dark.background = Some("#171615".into());
    dark.sidebar = Some("#201e1c".into());
    dark.sidebar_accent = Some("#302d2a".into());
    dark.secondary = dark.sidebar_accent.clone();
    dark.secondary_hover = Some("#3a3632".into());
    dark.secondary_active = dark.secondary_hover.clone();
    dark.muted = Some("#2c2926".into());
    dark.muted_foreground = Some("#aaa49e".into());
    dark.border = Some("#34312e".into());
    dark.sidebar_border = dark.border.clone();
    dark.popover = Some("#252220".into());
    dark.accent = dark.sidebar_accent.clone();

    let light = &mut Rc::make_mut(&mut theme.light_theme).colors;
    light.background = Some("#f9f9f9".into());
    light.sidebar = Some("#f4f4f4".into());
    light.sidebar_accent = Some("#dfdfdf".into());
    light.secondary = Some("#efefef".into());
    light.secondary_hover = Some("#eaeaea".into());
    light.secondary_active = light.sidebar_accent.clone();
    light.muted = light.secondary.clone();
    light.muted_foreground = Some("#606060".into());
    light.border = Some("#dfdfdf".into());
    light.sidebar_border = light.border.clone();
    light.popover = Some("#fafafa".into());
    light.accent = light.secondary.clone();

    let active = if theme.is_dark() {
        theme.dark_theme.clone()
    } else {
        theme.light_theme.clone()
    };
    theme.apply_config(&active);
}

pub(crate) fn control_text_size() -> Rems {
    rems(0.8125)
}

pub(crate) fn keyboard_scroll_viewport(
    id: impl Into<ElementId>,
    scroll: &UniformListScrollHandle,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let id = id.into();
    let focus = window
        .use_keyed_state(id.clone(), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    let scroll = scroll.0.borrow().base_handle.clone();
    let ring = cx.theme().ring;
    div()
        .id(id)
        .track_focus(&focus)
        .tab_stop(true)
        .border_1()
        .border_color(transparent_black())
        .focus(move |style| style.border_color(ring))
        .on_key_down(move |event, window, cx| {
            if event.keystroke.modifiers.modified() || scroll.max_offset().height <= px(0.) {
                return;
            }
            let offset = scroll.offset();
            let page = scroll.bounds().size.height;
            let y = match event.keystroke.key.as_str() {
                "pageup" => offset.y + page,
                "pagedown" => offset.y - page,
                "home" => px(0.),
                "end" => -scroll.max_offset().height,
                _ => return,
            };
            window.focus(&focus);
            scroll.set_offset(point(offset.x, y));
            window.refresh();
            cx.stop_propagation();
        })
}

pub fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

pub fn section_title(text: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

pub fn label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(control_text_size())
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

pub fn hint(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(control_text_size())
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

pub fn field(name: &'static str, state: &Entity<InputState>) -> Div {
    v_flex()
        .gap_1()
        .child(label(name))
        .child(control!(name, Input::new(state)))
}

pub fn choice(id: impl Into<ElementId>, title: impl Into<SharedString>, selected: bool) -> Button {
    Button::new(id)
        .small()
        .label(title)
        .selected(selected)
        .when(selected, |button| button.primary())
}
