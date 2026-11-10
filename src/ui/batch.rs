//! The batch dialog: a checklist of links added at once, from a list (paste, drop, import, command line), a
//! wildcard pattern, or the links on a web page.

use std::ops::Range;

use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    form::{Field, Form},
    h_flex,
    input::{Input, InputEvent, InputState},
    radio::{Radio, RadioGroup},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::engine;
use crate::links;
use crate::ui::settings::AppSettings;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BatchKind {
    /// Links handed over as they are.
    List,
    /// One address with `[1-50]`-style ranges.
    Pattern,
    /// The links found on a page.
    Page,
}

impl BatchKind {
    pub fn title(self) -> &'static str {
        match self {
            BatchKind::List => "Add links",
            BatchKind::Pattern => "Batch download",
            BatchKind::Page => "Grab links from a page",
        }
    }
}

/// Sent by Add with the checked links and the folder to save them to.
pub struct BatchSubmit {
    pub urls: Vec<String>,
    pub folder: String,
}

pub struct BatchAdd {
    kind: BatchKind,
    links: Vec<SharedString>,
    /// Parallel to `links`.
    checked: Vec<bool>,
    pattern: Entity<InputState>,
    page: Entity<InputState>,
    types: Entity<InputState>,
    /// How many levels of same-site pages the grabber follows.
    depth: usize,
    folder: Entity<InputState>,
    /// Why there are no links to show.
    message: Option<SharedString>,
    grabbing: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<BatchSubmit> for BatchAdd {}

impl BatchAdd {
    pub fn new(
        kind: BatchKind,
        links: Vec<String>,
        folder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let pattern =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/photos/img[001-120].jpg"));
        let page = cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/downloads"));
        let types_value = AppSettings::get(cx).watch_types.clone();
        let types = cx.new(|cx| InputState::new(window, cx).default_value(types_value));
        let folder = cx.new(|cx| InputState::new(window, cx).default_value(folder));
        let subscriptions = vec![
            cx.subscribe(&pattern, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.expand(cx);
                }
            }),
            cx.subscribe(&page, |this, _, event, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.grab(cx);
                }
            }),
        ];
        let first = match kind {
            BatchKind::List => None,
            BatchKind::Pattern => Some(&pattern),
            BatchKind::Page => Some(&page),
        };
        if let Some(input) = first {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
        let mut this = Self {
            kind,
            links: Vec::new(),
            checked: Vec::new(),
            pattern,
            page,
            types,
            depth: 1,
            folder,
            message: None,
            grabbing: None,
            _subscriptions: subscriptions,
        };
        this.set_links(links);
        this
    }

    fn set_links(&mut self, links: Vec<String>) {
        self.checked = vec![true; links.len()];
        self.links = links.into_iter().map(Into::into).collect();
    }

    fn expand(&mut self, cx: &mut Context<Self>) {
        let pattern = self.pattern.read(cx).value().trim().to_string();
        let (links, message) = match links::expand_pattern(&pattern) {
            _ if pattern.is_empty() => (Vec::new(), None),
            Ok(links) => (links, None),
            Err(message) => (Vec::new(), Some(message.into())),
        };
        self.set_links(links);
        self.message = message;
        cx.notify();
    }

    fn grab(&mut self, cx: &mut Context<Self>) {
        let page = self.page.read(cx).value().trim().to_string();
        let types = self.types.read(cx).value().to_string();
        let depth = self.depth;
        self.set_links(Vec::new());
        self.message = None;
        // Replacing the task drops (cancels) a grab still running.
        self.grabbing = Some(cx.spawn(async move |this, cx| {
            let result = match engine::RUNTIME.spawn(async move { links::grab(&page, depth).await }).await {
                Ok(result) => result,
                Err(e) => Err(e.to_string()),
            };
            this.update(cx, |this, cx| {
                this.grabbing = None;
                match result {
                    Ok(found) => {
                        let any_type = types.trim().is_empty();
                        let found: Vec<String> =
                            found.into_iter().filter(|link| any_type || links::has_type(link, &types)).collect();
                        if found.is_empty() {
                            this.message = Some("No links of those file types on that page.".into());
                        }
                        this.set_links(found);
                    }
                    Err(message) => this.message = Some(message.into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let urls = self.links.iter().zip(&self.checked).filter(|(_, c)| **c).map(|(l, _)| l.to_string()).collect();
        let folder = self.folder.read(cx).value().trim().to_string();
        cx.emit(BatchSubmit { urls, folder });
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<impl IntoElement + use<>> {
        range
            .map(|ix| {
                let link = self.links[ix].clone();
                Checkbox::new(ElementId::Name(link.clone()))
                    .small()
                    .w_full()
                    .px_2()
                    .checked(self.checked[ix])
                    .child(div().min_w_0().truncate().child(link))
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.checked[ix] = *checked;
                        cx.notify();
                    }))
            })
            .collect()
    }

    fn render_checklist(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let total = self.links.len();
        let selected = self.checked.iter().filter(|c| **c).count();
        let theme = cx.theme();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Checkbox::new("all-links")
                            .small()
                            .label(if total == 1 { "1 link".to_string() } else { format!("{total} links") })
                            .checked(selected == total)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.checked.fill(*checked);
                                cx.notify();
                            })),
                    )
                    .when(selected < total, |this| {
                        this.child(
                            div().text_xs().text_color(theme.muted_foreground).child(format!("{selected} selected")),
                        )
                    }),
            )
            .child(
                uniform_list("links", total, cx.processor(|this, range, _, cx| this.render_rows(range, cx)))
                    .h(rems(14.))
                    .py_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border),
            )
    }
}

impl Render for BatchAdd {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.checked.iter().filter(|c| **c).count();
        let muted = cx.theme().muted_foreground;
        let add_label = match selected {
            1 => "Add 1 download".to_string(),
            n => format!("Add {n} downloads"),
        };
        v_flex()
            .gap_4()
            .child(
                Form::new()
                    .when(self.kind == BatchKind::Pattern, |this| {
                        this.child(
                            Field::new()
                                .label("Address")
                                .description("Put [1-50], [001-120] or [a-z] where the address changes.")
                                .child(Input::new(&self.pattern).id("pattern").small().prefix(IconName::Globe)),
                        )
                    })
                    .when(self.kind == BatchKind::Page, |this| {
                        this.child(
                            Field::new().label("Page").child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(Input::new(&self.page).small().prefix(IconName::Globe)))
                                    .child(
                                        Button::new("find-links")
                                            .small()
                                            .outline()
                                            .label("Find links")
                                            .loading(self.grabbing.is_some())
                                            .on_click(cx.listener(|this, _, _, cx| this.grab(cx))),
                                    ),
                            ),
                        )
                        .child(
                            Field::new().label("Follow links on the same site").child(
                                RadioGroup::horizontal("depth")
                                    .selected_index(Some(self.depth))
                                    .children(
                                        ["Don't follow", "1 level", "2 levels"]
                                            .map(|label| Radio::new(SharedString::from(label)).label(label)),
                                    )
                                    .on_change(cx.listener(|this, ix: &usize, _, cx| {
                                        this.depth = *ix;
                                        cx.notify();
                                    })),
                            ),
                        )
                        .child(
                            Field::new()
                                .label("File types")
                                .description("Extensions, separated by spaces. Leave empty for every link.")
                                .child(Input::new(&self.types).small()),
                        )
                    }),
            )
            .when(self.grabbing.is_some(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(muted)
                        .child(Spinner::new().small())
                        .child("Reading the page…"),
                )
            })
            .when_some(self.message.clone(), |this, message| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(message))
            })
            .when(!self.links.is_empty(), |this| this.child(self.render_checklist(cx)))
            .child(
                Form::new().child(
                    Field::new().label("Save to").child(Input::new(&self.folder).small().prefix(IconName::Folder)),
                ),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-batch")
                            .small()
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("add-batch")
                            .small()
                            .primary()
                            .label(add_label)
                            .disabled(selected == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext as _, Bounds, ElementId, TestAppContext, WindowBounds, WindowOptions, px, size};

    use super::{BatchAdd, BatchKind, BatchSubmit};
    use crate::ui::settings::AppSettings;

    #[gpui_kit::test]
    fn pattern_previews_and_adds_the_checked_links(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(AppSettings::default());
        });
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(640.), px(800.))))),
            ..Default::default()
        };
        let (handle, view) = cx
            .update(|cx| {
                gpui_kit::open_window(options, cx, |window, cx| {
                    cx.new(|cx| BatchAdd::new(BatchKind::Pattern, Vec::new(), "/tmp/batch".into(), window, cx))
                })
            })
            .expect("open test window");
        let submitted = Rc::new(RefCell::new(None));
        let sink = submitted.clone();
        cx.update(|cx| {
            cx.subscribe(&view, move |_, BatchSubmit { urls, folder }, _| {
                *sink.borrow_mut() = Some((urls.clone(), folder.clone()));
            })
            .detach()
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            // Nothing to add until the pattern makes links.
            window.click("add-batch", cx);
            assert!(submitted.borrow().is_none());

            window.input("https://x.com/f[08-10].zip", cx);
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(ElementId::Name("https://x.com/f09.zip".into())).visible());
            // Unchecking one leaves it out.
            window.click(ElementId::Name("https://x.com/f09.zip".into()), cx);
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("add-batch", cx);
        })
        .unwrap();
        let (urls, folder) = submitted.borrow_mut().take().expect("submitted");
        assert_eq!(urls, ["https://x.com/f08.zip", "https://x.com/f10.zip"]);
        assert_eq!(folder, "/tmp/batch");

        cx.update_window(handle, |_, window, cx| {
            window.click("pattern", cx);
            window.press("secondary-a", cx);
            window.input("https://x.com/f.zip", cx);
        })
        .unwrap();
        cx.update(|cx| assert!(view.read(cx).links.is_empty() && view.read(cx).message.is_some()));
    }
}
