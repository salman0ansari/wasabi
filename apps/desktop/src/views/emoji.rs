//! Local Unicode emoji catalog and composer picker.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{Animation, AnimationExt, ClickEvent, Window, ease_out_quint, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::InputState;
use gpui_component::popover::Popover;
use gpui_component::{Disableable as _, Selectable as _, tooltip::Tooltip};

use crate::theme;
use crate::views::root::MainWindow;

const PICKER_W: f32 = 560.0;
const PICKER_H: f32 = 549.0;
const EMOJI_CELL: f32 = 40.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EmojiCategory {
    Smileys,
    People,
    Nature,
    Food,
    Activities,
    Travel,
    Objects,
    Symbols,
}

impl EmojiCategory {
    pub const ALL: [Self; 8] = [
        Self::Smileys,
        Self::People,
        Self::Nature,
        Self::Food,
        Self::Activities,
        Self::Travel,
        Self::Objects,
        Self::Symbols,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Smileys => "Smileys",
            Self::People => "People",
            Self::Nature => "Nature",
            Self::Food => "Food",
            Self::Activities => "Activities",
            Self::Travel => "Travel",
            Self::Objects => "Objects",
            Self::Symbols => "Symbols",
        }
    }

    pub fn tab_glyph(self) -> &'static str {
        match self {
            Self::Smileys => "😊",
            Self::People => "👋",
            Self::Nature => "🌿",
            Self::Food => "🍔",
            Self::Activities => "⚽",
            Self::Travel => "✈️",
            Self::Objects => "💡",
            Self::Symbols => "❤️",
        }
    }

    pub fn emoji(self) -> &'static [&'static str] {
        match self {
            Self::Smileys => SMILEYS,
            Self::People => PEOPLE,
            Self::Nature => NATURE,
            Self::Food => FOOD,
            Self::Activities => ACTIVITIES,
            Self::Travel => TRAVEL,
            Self::Objects => OBJECTS,
            Self::Symbols => SYMBOLS,
        }
    }
}

const SMILEYS: &[&str] = &[
    "😀", "😃", "😄", "😁", "😆", "😅", "😂", "🤣", "😊", "😇", "🙂", "🙃", "😉", "😌", "😍", "🥰",
    "😘", "😗", "😙", "😚", "😋", "😛", "😝", "😜", "🤪", "🤨", "🧐", "🤓", "😎", "🤩", "🥳", "😏",
    "😒", "😞", "😔", "😟", "😕", "🙁", "☹️", "😣", "😖", "😫", "😩", "🥺", "😢", "😭", "😤", "😠",
    "😡", "🤬", "🤯", "😳", "🥵", "🥶", "😱", "😨", "😰", "😥", "😓", "🤗", "🤔", "🤭", "🤫", "🤥",
    "😶", "😐", "😑", "😬", "🙄", "😯", "😦", "😧", "😮", "😲", "🥱", "😴", "🤤", "😪", "😵", "🤐",
    "🥴", "🤢", "🤮", "🤧", "😷", "🤒", "🤕", "🤑", "🤠", "😈", "👿", "👹", "👺", "🤡", "💩", "👻",
    "💀", "☠️", "👽", "👾", "🤖", "🎃", "😺", "😸", "😹", "😻", "😼", "😽", "🙀", "😿", "😾",
];

const PEOPLE: &[&str] = &[
    "👋", "🤚", "🖐️", "✋", "🖖", "👌", "🤌", "🤏", "✌️", "🤞", "🤟", "🤘", "🤙", "👈", "👉", "👆",
    "🖕", "👇", "☝️", "👍", "👎", "✊", "👊", "🤛", "🤜", "👏", "🙌", "👐", "🤲", "🤝", "🙏", "✍️",
    "💅", "🤳", "💪", "🦾", "🦿", "🦵", "🦶", "👂", "👃", "👀", "👁️", "👅", "👄", "💋", "👶", "👧",
    "🧒", "👦", "👩", "🧑", "👨", "👵", "🧓", "👴", "🙍", "🙎", "🙅", "🙆", "💁", "🙋", "🧏", "🙇",
    "🤦", "🤷", "👮", "🕵️", "💂", "🥷", "👷", "🤴", "👸", "👳", "👲", "🧕", "🤵", "👰", "🤰", "🤱",
    "👼", "🎅", "🤶", "🦸", "🦹", "🧙", "🧚", "🧛", "🧜", "🧝", "🧞", "🧟", "💆", "💇", "🚶", "🧍",
    "🧎", "🏃", "💃", "🕺", "🕴️", "👯", "🧖",
];

const NATURE: &[&str] = &[
    "🐶", "🐱", "🐭", "🐹", "🐰", "🦊", "🐻", "🐼", "🐨", "🐯", "🦁", "🐮", "🐷", "🐽", "🐸", "🐵",
    "🙈", "🙉", "🙊", "🐔", "🐧", "🐦", "🐤", "🐣", "🐥", "🦆", "🦅", "🦉", "🦇", "🐺", "🐗", "🐴",
    "🦄", "🐝", "🪱", "🐛", "🦋", "🐌", "🐞", "🐜", "🪰", "🪲", "🪳", "🦟", "🦗", "🕷️", "🕸️", "🦂",
    "🐢", "🐍", "🦎", "🦖", "🦕", "🐙", "🦑", "🦐", "🦞", "🦀", "🐡", "🐠", "🐟", "🐬", "🐳", "🐋",
    "🦈", "🐊", "🐅", "🐆", "🦓", "🦍", "🦧", "🐘", "🦛", "🦏", "🐪", "🐫", "🦒", "🦘", "🦬", "🐃",
    "🐂", "🐄", "🐎", "🐖", "🐏", "🐑", "🦙", "🐐", "🦌", "🐕", "🐩", "🦮", "🐈", "🐓", "🦃", "🦚",
    "🦜", "🦢", "🦩", "🕊️", "🐇", "🦝", "🦨", "🦡", "🦫", "🦦", "🦥", "🐁", "🐀", "🐿️", "🦔", "🐾",
    "🌸", "🌹", "🥀", "🌺", "🌻", "🌼", "🌷", "🌱", "🪴", "🌲", "🌳", "🌴", "🌵", "🌾", "🌿", "☘️",
    "🍀", "🍁", "🍂", "🍃", "🍄", "🌰", "🌍", "🌎", "🌏", "🌑", "🌓", "🌕", "🌙", "⭐", "🌟", "✨",
    "⚡", "🔥", "💥", "☄️", "☀️", "🌤️", "⛅", "🌧️", "⛈️", "❄️", "☃️", "⛄", "🌬️", "💨", "🌪️", "🌈",
    "☔", "💧", "🌊",
];

const FOOD: &[&str] = &[
    "🍏", "🍎", "🍐", "🍊", "🍋", "🍌", "🍉", "🍇", "🍓", "🫐", "🍈", "🍒", "🍑", "🥭", "🍍", "🥥",
    "🥝", "🍅", "🍆", "🥑", "🥦", "🥬", "🥒", "🌶️", "🫑", "🌽", "🥕", "🫒", "🧄", "🧅", "🥔", "🍠",
    "🫘", "🥜", "🍞", "🥐", "🥖", "🫓", "🥨", "🥯", "🥞", "🧇", "🧀", "🍖", "🍗", "🥩", "🥓", "🍔",
    "🍟", "🍕", "🌭", "🥪", "🌮", "🌯", "🫔", "🥙", "🧆", "🥚", "🍳", "🥘", "🍲", "🫕", "🥣", "🥗",
    "🍿", "🧈", "🧂", "🥫", "🍱", "🍘", "🍙", "🍚", "🍛", "🍜", "🍝", "🍢", "🍣", "🍤", "🍥", "🥮",
    "🍡", "🥟", "🥠", "🥡", "🦪", "🍦", "🍧", "🍨", "🍩", "🍪", "🎂", "🍰", "🧁", "🥧", "🍫", "🍬",
    "🍭", "🍮", "🍯", "🍼", "🥛", "☕", "🍵", "🧃", "🥤", "🧋", "🍶", "🍺", "🍻", "🥂", "🍷", "🥃",
    "🍸", "🍹", "🧉", "🍾", "🧊", "🥄", "🍴", "🍽️", "🥢", "🫙",
];

const ACTIVITIES: &[&str] = &[
    "⚽", "🏀", "🏈", "⚾", "🥎", "🎾", "🏐", "🏉", "🥏", "🎱", "🪀", "🏓", "🏸", "🏒", "🏑", "🥍",
    "🏏", "🥅", "⛳", "🪁", "🏹", "🎣", "🤿", "🥊", "🥋", "🎽", "🛹", "🛼", "🛷", "⛸️", "🥌", "🎿",
    "⛷️", "🏂", "🪂", "🏋️", "🤼", "🤸", "⛹️", "🤺", "🤾", "🏌️", "🏇", "🧘", "🏄", "🏊", "🤽", "🚣",
    "🧗", "🚴", "🚵", "🏆", "🥇", "🥈", "🥉", "🏅", "🎖️", "🏵️", "🎗️", "🎫", "🎟️", "🎪", "🤹", "🎭",
    "🩰", "🎨", "🎬", "🎤", "🎧", "🎼", "🎹", "🥁", "🪘", "🎷", "🎺", "🎸", "🪕", "🎻", "🎲", "♟️",
    "🎯", "🎳", "🎮", "🕹️", "🎰", "🧩",
];

const TRAVEL: &[&str] = &[
    "🚗", "🚕", "🚙", "🚌", "🚎", "🏎️", "🚓", "🚑", "🚒", "🚐", "🛻", "🚚", "🚛", "🚜", "🦯", "🦽",
    "🦼", "🛴", "🚲", "🛵", "🏍️", "🛺", "🚨", "🚔", "🚍", "🚘", "🚖", "🚡", "🚠", "🚟", "🚃", "🚋",
    "🚞", "🚝", "🚄", "🚅", "🚈", "🚂", "🚆", "🚇", "🚊", "🚉", "✈️", "🛫", "🛬", "🛩️", "💺", "🛰️",
    "🚀", "🛸", "🚁", "🛶", "⛵", "🚤", "🛥️", "🛳️", "⛴️", "🚢", "⚓", "🪝", "⛽", "🚧", "🚦", "🚥",
    "🗺️", "🗿", "🗽", "🗼", "🏰", "🏯", "🏟️", "🎡", "🎢", "🎠", "⛲", "⛱️", "🏖️", "🏝️", "🏜️", "🌋",
    "⛰️", "🏔️", "🗻", "🏕️", "⛺", "🛖", "🏠", "🏡", "🏘️", "🏚️", "🏗️", "🏭", "🏢", "🏬", "🏣", "🏤",
    "🏥", "🏦", "🏨", "🏪", "🏫", "🏩", "💒", "🏛️", "⛪", "🕌", "🕍", "🛕", "🕋", "⛩️", "🛤️", "🛣️",
    "🗾", "🎑", "🏞️", "🌅", "🌄", "🌠", "🎇", "🎆", "🌇", "🌆", "🏙️", "🌃", "🌌", "🌉", "🌁",
];

const OBJECTS: &[&str] = &[
    "⌚", "📱", "📲", "💻", "⌨️", "🖥️", "🖨️", "🖱️", "🖲️", "💽", "💾", "💿", "📀", "📼", "📷", "📸",
    "📹", "🎥", "📽️", "📞", "☎️", "📟", "📠", "📺", "📻", "🎙️", "🎚️", "🎛️", "🧭", "⏱️", "⏲️", "⏰",
    "🕰️", "⌛", "⏳", "📡", "🔋", "🔌", "💡", "🔦", "🕯️", "🪔", "🧯", "🛢️", "💸", "💵", "💴", "💶",
    "💷", "🪙", "💰", "💳", "💎", "⚖️", "🪜", "🧰", "🪛", "🔧", "🔨", "⚒️", "🛠️", "⛏️", "🪚", "🔩",
    "⚙️", "🪤", "🧱", "⛓️", "🧲", "🔫", "💣", "🧨", "🪓", "🔪", "🗡️", "⚔️", "🛡️", "🚬", "⚰️", "🪦",
    "⚱️", "🏺", "🔮", "📿", "🧿", "💈", "⚗️", "🔭", "🔬", "🕳️", "🩹", "🩺", "💊", "💉", "🩸", "🧬",
    "🦠", "🧫", "🧪", "🌡️", "🧹", "🪠", "🧺", "🧻", "🚽", "🚰", "🚿", "🛁", "🛀", "🧴", "🧷", "🧼",
    "🪥", "🧽", "🛒", "🚪", "🪞", "🪟", "🛏️", "🛋️", "🪑", "🧸", "🖼️", "🛍️", "🎁", "🎈", "🎉", "🎊",
    "🎀", "🪄", "🪅", "🪆", "✉️", "📩", "📨", "📧", "💌", "📥", "📤", "📦", "🏷️", "🪧", "📪", "📫",
    "📬", "📭", "📮", "📯", "📜", "📃", "📄", "📑", "🧾", "📊", "📈", "📉", "🗒️", "🗓️", "📆", "📅",
    "🗑️", "📇", "🗃️", "🗳️", "🗄️", "📋", "📁", "📂", "🗂️", "🗞️", "📰", "📓", "📔", "📒", "📕", "📗",
    "📘", "📙", "📚", "📖", "🔖", "🔗", "📎", "🖇️", "📐", "📏", "🧮", "📌", "📍", "✂️", "🖊️", "🖋️",
    "✒️", "🖌️", "🖍️", "📝", "✏️", "🔍", "🔎", "🔏", "🔐", "🔒", "🔓",
];

const SYMBOLS: &[&str] = &[
    "❤️", "🧡", "💛", "💚", "💙", "💜", "🖤", "🤍", "🤎", "💔", "❣️", "💕", "💞", "💓", "💗", "💖",
    "💘", "💝", "💟", "☮️", "✝️", "☪️", "🕉️", "☸️", "✡️", "🔯", "🕎", "☯️", "☦️", "🛐", "⛎", "♈",
    "♉", "♊", "♋", "♌", "♍", "♎", "♏", "♐", "♑", "♒", "♓", "🆔", "⚛️", "🉑", "☢️", "☣️",
    "📴", "📳", "🈶", "🈚", "🈸", "🈺", "🈷️", "✴️", "🆚", "💮", "🉐", "㊙️", "㊗️", "🈴", "🈵", "🈹",
    "🈲", "🅰️", "🅱️", "🆎", "🆑", "🅾️", "🆘", "❌", "⭕", "🛑", "⛔", "📛", "🚫", "💯", "💢", "♨️",
    "🚷", "🚯", "🚳", "🚱", "🔞", "📵", "🚭", "❗", "❕", "❓", "❔", "‼️", "⁉️", "🔅", "🔆", "⚠️",
    "🚸", "🔱", "⚜️", "🔰", "♻️", "✅", "🈯", "💹", "❇️", "✳️", "❎", "🌐", "💠", "Ⓜ️", "🌀", "💤",
    "🏧", "🚾", "♿", "🅿️", "🛗", "🈳", "🈂️", "🛂", "🛃", "🛄", "🛅", "🚹", "🚺", "🚼", "🚻", "🚮",
    "🎦", "📶", "🈁", "🔣", "ℹ️", "🔤", "🔡", "🔠", "🆖", "🆗", "🆙", "🆒", "🆕", "🆓", "0️⃣", "1️⃣",
    "2️⃣", "3️⃣", "4️⃣", "5️⃣", "6️⃣", "7️⃣", "8️⃣", "9️⃣", "🔟", "🔢", "#️⃣", "*️⃣", "🔴", "🟠", "🟡", "🟢",
    "🔵", "🟣", "⚫", "⚪", "🟤", "🔺", "🔻", "🔸", "🔹", "🔶", "🔷", "🔳", "🔲", "▪️", "▫️", "◾",
    "◽", "◼️", "◻️", "🟥", "🟧", "🟨", "🟩", "🟦", "🟪", "⬛", "⬜", "🟫", "🔈", "🔇", "🔉", "🔊",
    "🔔", "🔕", "📣", "📢", "💬", "💭", "🗯️", "♠️", "♣️", "♥️", "♦️", "🃏", "🎴", "🀄", "🎵", "🎶",
    "➕", "➖", "➗", "✖️", "♾️", "💲", "💱", "™️", "©️", "®️", "〰️", "➰", "➿", "✔️", "☑️", "🔘",
];

/// Insert `emoji` at the composer caret and keep the input focused for typing.
pub fn insert_into_composer(
    input: &gpui::Entity<InputState>,
    emoji: &str,
    window: &mut Window,
    cx: &mut gpui::Context<MainWindow>,
) {
    input.update(cx, |input, cx| {
        input.insert(emoji, window, cx);
        input.focus(window, cx);
    });
}

/// Insert `emoji` into `value` after `offset` Unicode scalar values.
#[cfg(test)]
pub fn insert_at_char_offset(value: &str, offset: usize, emoji: &str) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(offset).collect();
    let suffix: String = chars.collect();
    format!("{prefix}{emoji}{suffix}")
}

pub fn picker_button(
    this: &mut MainWindow,
    can_compose: bool,
    window: &mut Window,
    cx: &mut gpui::Context<MainWindow>,
) -> gpui::AnyElement {
    let mut button = Button::new("emoji-button")
        .label("😊")
        .ghost()
        .tooltip("Insert emoji")
        .disabled(!can_compose)
        .selected(this.emoji_picker_open);
    if !can_compose {
        this.emoji_picker_open = false;
        return button.into_any_element();
    }

    button = button.tab_stop(false);
    let mut picker = Popover::new("composer-emoji-picker")
        .anchor(gpui::Anchor::BottomLeft)
        .appearance(false)
        .overlay_closable(true)
        .open(this.emoji_picker_open)
        .on_open_change(window.listener_for(&cx.entity(), {
            move |this, open: &bool, window, cx| {
                this.emoji_picker_open = *open;
                this.composer_input
                    .update(cx, |input, cx| input.focus(window, cx));
                cx.notify();
            }
        }))
        .trigger(button);
    if this.emoji_picker_open {
        picker = picker.child(picker_panel(this, cx));
    }
    picker.into_any_element()
}

fn picker_panel(this: &MainWindow, cx: &mut gpui::Context<MainWindow>) -> gpui::AnyElement {
    let active = this.emoji_category;
    let panel = gpui::div()
        .id("emoji-picker")
        .occlude()
        .w(px(PICKER_W))
        .h(px(PICKER_H))
        .rounded(px(theme::RADIUS_LG))
        .overflow_hidden()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_elevated())
        .shadow(theme::overlay_shadow())
        .flex()
        .flex_col()
        .child(
            gpui::div()
                .px(px(8.0))
                .pt(px(8.0))
                .pb(px(4.0))
                .child(category_tabs(active, cx)),
        )
        .child(
            gpui::div()
                .px(px(12.0))
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text_secondary())
                .child(active.label()),
        )
        .child(
            gpui::div()
                .flex_1()
                .min_h(px(0.0))
                .px(px(8.0))
                .pb(px(8.0))
                .child(emoji_grid(active, cx)),
        );
    if this.settings.reduce_motion {
        panel.into_any_element()
    } else {
        panel
            .with_animation(
                "emoji-picker-entrance",
                Animation::new(Duration::from_millis(theme::MOTION_STANDARD_MS))
                    .with_easing(ease_out_quint()),
                |panel, progress| panel.opacity(0.55 + 0.45 * progress),
            )
            .into_any_element()
    }
}

fn category_tabs(active: EmojiCategory, cx: &mut gpui::Context<MainWindow>) -> gpui::Div {
    gpui::div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(2.0))
        .children(EmojiCategory::ALL.into_iter().map(|category| {
            let selected = category == active;
            gpui::div()
                .id(("emoji-category", category as usize))
                .flex_1()
                .h(px(theme::ACTION_SIZE))
                .rounded(px(theme::RADIUS_MD))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .aria_label(category.label())
                .tooltip(move |window, cx| Tooltip::new(category.label()).build(window, cx))
                .when(selected, |tab| tab.bg(theme::surface_emphasized()))
                .hover(|tab| tab.bg(theme::row_hover()))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.emoji_category = category;
                    this.composer_input
                        .update(cx, |input, cx| input.focus(window, cx));
                    cx.notify();
                }))
                .child(gpui::div().text_size(px(18.0)).child(category.tab_glyph()))
        }))
}

fn emoji_grid(
    category: EmojiCategory,
    cx: &mut gpui::Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id("emoji-grid")
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scroll()
        .child(
            gpui::div().flex().flex_wrap().children(
                category
                    .emoji()
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, emoji)| emoji_cell(category, index, emoji, cx)),
            ),
        )
}

fn emoji_cell(
    category: EmojiCategory,
    index: usize,
    emoji: &'static str,
    cx: &mut gpui::Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(("emoji-cell", (category as usize) * 1_000 + index))
        .size(px(EMOJI_CELL))
        .rounded(px(theme::RADIUS_SM))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .aria_label(emoji)
        .hover(|cell| cell.bg(theme::row_hover()))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            insert_into_composer(&this.composer_input, emoji, window, cx);
        }))
        .child(gpui::div().text_size(px(22.0)).child(emoji))
}

#[cfg(test)]
mod tests {
    use super::{EmojiCategory, insert_at_char_offset};
    use std::collections::HashSet;

    #[test]
    fn category_lists_are_non_empty_unique_and_non_blank() {
        let mut seen = HashSet::new();
        let mut labels = HashSet::new();
        let mut total = 0usize;
        for category in EmojiCategory::ALL {
            let label = category.label();
            assert!(!label.is_empty(), "{category:?} label");
            assert!(labels.insert(label), "duplicate category label {label}");
            let list = category.emoji();
            assert!(!list.is_empty(), "{category:?} is empty");
            let mut local = HashSet::new();
            for emoji in list {
                assert!(!emoji.is_empty(), "{category:?} contains an empty glyph");
                assert!(
                    !emoji.chars().any(char::is_whitespace),
                    "{category:?} glyph {emoji:?} contains whitespace"
                );
                assert!(local.insert(*emoji), "{category:?} repeats {emoji}");
                assert!(
                    seen.insert(*emoji),
                    "duplicate emoji {emoji} across categories"
                );
                total += 1;
            }
        }
        assert!(
            total >= 200,
            "expected a few hundred common emoji, got {total}"
        );
    }

    #[test]
    fn insert_helper_places_emoji_at_the_character_offset() {
        assert_eq!(insert_at_char_offset("hi", 2, "🎉"), "hi🎉");
        assert_eq!(insert_at_char_offset("héllo", 1, "👋"), "h👋éllo");
        assert_eq!(insert_at_char_offset("", 0, "😊"), "😊");
    }
}
