//! The multi-document shell: a strip of tabs along the top of the window,
//! one per open document, with the active document's whole workbench below.
//!
//! Every tab is a complete [`KernelLabApp`]: its own model, sketch, history,
//! selection, and view. Switching tabs swaps which one is drawn, so a
//! document keeps its camera and its staged work while another is in front.
//! The shell owns nothing of the model; it only decides which document is
//! live and answers the few requests a document cannot answer for itself.

use egui::{Color32, FontId, Frame, Margin, RichText, Stroke};

use crate::{KernelLabApp, theme};

/// Something a document asks of the shell that hosts it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellRequest {
    /// Open a blank document in a new tab and make it active.
    NewDocument,
    /// Close the document that made the request.
    CloseDocument,
}

/// The height of the tab strip. The header below it reserves its own
/// height, so the strip is a fixed band that never moves the viewport.
pub const TAB_STRIP_HEIGHT: f32 = 30.0;

/// One open document.
struct DocumentTab {
    app: KernelLabApp,
}

/// The application: every open document, and which one is in front.
pub struct WorkbenchShell {
    documents: Vec<DocumentTab>,
    active: usize,
    next_serial: usize,
}

impl WorkbenchShell {
    /// The running application, starting with one blank document.
    #[must_use]
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        Self::with_first_document(KernelLabApp::new(creation_context))
    }

    /// The application for a test harness: its first document is the
    /// paused workbench the UI suites drive frame by frame.
    #[must_use]
    pub fn new_paused(creation_context: &eframe::CreationContext<'_>) -> Self {
        Self::with_first_document(KernelLabApp::new_paused(creation_context))
    }

    /// A shell around one already-built document, which becomes "Document 1".
    #[must_use]
    pub fn with_first_document(mut app: KernelLabApp) -> Self {
        app.set_document_title("Document 1");
        Self {
            documents: vec![DocumentTab { app }],
            active: 0,
            next_serial: 2,
        }
    }

    /// How many documents are open.
    #[must_use]
    pub fn document_count(&self) -> usize {
        self.documents.len()
    }

    /// The index of the document in front.
    #[must_use]
    pub const fn active_index(&self) -> usize {
        self.active
    }

    /// The document in front.
    #[must_use]
    pub fn active_document(&self) -> &KernelLabApp {
        &self.documents[self.active].app
    }

    pub fn active_document_mut(&mut self) -> &mut KernelLabApp {
        &mut self.documents[self.active].app
    }

    /// The titles of every open document, in tab order.
    #[must_use]
    pub fn titles(&self) -> Vec<String> {
        self.documents
            .iter()
            .map(|tab| tab.app.document_title().to_owned())
            .collect()
    }

    /// Opens `app` as a new tab, numbers it, and brings it to the front.
    pub fn open_document(&mut self, mut app: KernelLabApp) -> usize {
        let serial = self.next_serial;
        self.next_serial += 1;
        app.set_document_title(format!("Document {serial}"));
        // A new document saves beside the first one rather than over it.
        let sibling = self
            .documents
            .first()
            .map(|tab| tab.app.document_path().to_path_buf());
        if let Some(sibling) = sibling {
            app.set_document_path(sibling.with_file_name(format!("document-{serial}.artificer")));
        }
        self.documents.push(DocumentTab { app });
        self.active = self.documents.len() - 1;
        self.active
    }

    /// Brings the document at `index` to the front.
    pub fn activate(&mut self, index: usize) -> bool {
        if index < self.documents.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// Moves to the next tab, wrapping around.
    pub fn activate_next(&mut self) {
        self.active = (self.active + 1) % self.documents.len();
    }

    /// Moves to the previous tab, wrapping around.
    pub fn activate_previous(&mut self) {
        self.active = (self.active + self.documents.len() - 1) % self.documents.len();
    }

    /// Closes the document at `index`. The last document is never closed;
    /// closing it is refused so the window always has a document to show.
    /// Returns the closed document so a caller can keep or discard it.
    pub fn close(&mut self, index: usize) -> Option<KernelLabApp> {
        if index >= self.documents.len() || self.documents.len() == 1 {
            return None;
        }
        let closed = self.documents.remove(index);
        if self.active > index || self.active == self.documents.len() {
            self.active -= 1;
        }
        Some(closed.app)
    }

    /// Answers the requests every document made this frame.
    fn service_requests(&mut self, egui_ctx: &egui::Context) {
        let mut open_new = 0;
        let mut close = Vec::new();
        for (index, tab) in self.documents.iter_mut().enumerate() {
            for request in tab.app.take_shell_requests() {
                match request {
                    ShellRequest::NewDocument => open_new += 1,
                    ShellRequest::CloseDocument => close.push(index),
                }
            }
        }
        // Close from the back so earlier indices stay valid.
        close.sort_unstable();
        for index in close.into_iter().rev() {
            self.close(index);
        }
        for _ in 0..open_new {
            self.open_document(KernelLabApp::new_document(egui_ctx));
        }
    }

    /// The keyboard: Ctrl+T opens a document, Ctrl+W closes the active one,
    /// Ctrl+Tab and Ctrl+Shift+Tab cycle, and Ctrl+1 to Ctrl+9 jump.
    fn handle_shortcuts(&mut self, egui_ctx: &egui::Context) {
        let command = egui::Modifiers::COMMAND;
        let (new_document, close_document, next, previous, jump) = egui_ctx.input_mut(|input| {
            let jump = [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
                egui::Key::Num8,
                egui::Key::Num9,
            ]
            .into_iter()
            .position(|key| input.consume_key(command, key));
            (
                input.consume_key(command, egui::Key::T),
                input.consume_key(command, egui::Key::W),
                input.consume_key(command, egui::Key::Tab),
                input.consume_key(command | egui::Modifiers::SHIFT, egui::Key::Tab),
                jump,
            )
        });
        if new_document {
            self.open_document(KernelLabApp::new_document(egui_ctx));
        }
        if close_document {
            self.close(self.active);
        }
        if previous {
            self.activate_previous();
        } else if next {
            self.activate_next();
        }
        if let Some(index) = jump {
            self.activate(index);
        }
    }

    /// Draws the tab strip and applies what was clicked.
    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let mut activate = None;
        let mut close = None;
        let mut open = false;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (index, tab) in self.documents.iter().enumerate() {
                let is_active = index == self.active;
                let title = tab.app.document_title().to_owned();
                let fill = if is_active {
                    theme::ribbon_fill()
                } else {
                    Color32::TRANSPARENT
                };
                let text_color = if is_active {
                    theme::text()
                } else {
                    theme::text().gamma_multiply(0.7)
                };
                let response = Frame::new()
                    .fill(fill)
                    .inner_margin(Margin::symmetric(10, 4))
                    .corner_radius(egui::CornerRadius {
                        nw: 6,
                        ne: 6,
                        sw: 0,
                        se: 0,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let label = ui.add(
                                egui::Label::new(
                                    RichText::new(&title)
                                        .font(FontId::proportional(13.0))
                                        .color(text_color),
                                )
                                .sense(egui::Sense::click()),
                            );
                            label.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    format!("Show {title}"),
                                )
                            });
                            if label.clicked() {
                                activate = Some(index);
                            }
                            if self.documents.len() > 1 {
                                let glyph = ui.add(
                                    egui::Label::new(
                                        RichText::new("×")
                                            .font(FontId::proportional(13.0))
                                            .color(text_color),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                glyph.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        true,
                                        format!("Close {title}"),
                                    )
                                });
                                if glyph.on_hover_text("Close this document").clicked() {
                                    close = Some(index);
                                }
                            }
                        });
                    })
                    .response;
                if is_active {
                    // The active tab joins the header below it: no bottom edge.
                    let rect = response.rect;
                    ui.painter().line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(1.0, theme::ribbon_fill()),
                    );
                }
            }
            ui.add_space(4.0);
            let add = ui
                .add(
                    egui::Label::new(
                        RichText::new("+")
                            .font(FontId::proportional(15.0))
                            .color(theme::accent()),
                    )
                    .sense(egui::Sense::click()),
                )
                .on_hover_text("New document (Ctrl+T)");
            add.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New document tab")
            });
            if add.clicked() {
                open = true;
            }
        });
        if let Some(index) = close {
            self.close(index);
        }
        if let Some(index) = activate {
            self.activate(index);
        }
        if open {
            let app = KernelLabApp::new_document(ui.ctx());
            self.open_document(app);
        }
    }
}

impl eframe::App for WorkbenchShell {
    fn logic(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        self.service_requests(context);
        self.handle_shortcuts(context);
        // Only the document in front runs its frame logic: background
        // documents keep their state exactly as they were left.
        <KernelLabApp as eframe::App>::logic(&mut self.documents[self.active].app, context, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        egui::Panel::top("document_tabs")
            .exact_size(TAB_STRIP_HEIGHT)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::panel().gamma_multiply(0.92))
                    .inner_margin(Margin {
                        left: 8,
                        right: 8,
                        top: 4,
                        bottom: 0,
                    })
                    .stroke(Stroke::new(0.0, Color32::TRANSPARENT)),
            )
            .show(ui, |ui| self.tab_strip(ui));
        <KernelLabApp as eframe::App>::ui(&mut self.documents[self.active].app, ui, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell() -> WorkbenchShell {
        WorkbenchShell::with_first_document(KernelLabApp::default())
    }

    #[test]
    fn the_first_document_is_document_one_and_the_only_tab() {
        let shell = shell();
        assert_eq!(shell.document_count(), 1);
        assert_eq!(shell.active_index(), 0);
        assert_eq!(shell.titles(), vec!["Document 1".to_owned()]);
    }

    #[test]
    fn opening_documents_numbers_them_and_brings_each_to_the_front() {
        let mut shell = shell();
        shell.open_document(KernelLabApp::default());
        shell.open_document(KernelLabApp::default());
        assert_eq!(
            shell.titles(),
            vec![
                "Document 1".to_owned(),
                "Document 2".to_owned(),
                "Document 3".to_owned()
            ]
        );
        assert_eq!(shell.active_index(), 2);
        // Each new document saves beside the first, never over it.
        let first = shell.documents[0].app.document_path().to_path_buf();
        let third = shell.documents[2].app.document_path().to_path_buf();
        assert_ne!(first, third);
        assert_eq!(first.parent(), third.parent());
        assert!(third.ends_with("document-3.artificer"));
    }

    #[test]
    fn switching_keeps_every_document_as_it_was_left() {
        let mut shell = shell();
        shell.open_document(KernelLabApp::default());
        shell.active_document_mut().set_document_title("Renamed");
        shell.activate(0);
        assert_eq!(shell.active_document().document_title(), "Document 1");
        shell.activate_next();
        assert_eq!(shell.active_document().document_title(), "Renamed");
        shell.activate_next();
        assert_eq!(shell.active_index(), 0, "cycling wraps around");
        shell.activate_previous();
        assert_eq!(shell.active_index(), 1);
        assert!(!shell.activate(7), "an index past the tabs is refused");
    }

    #[test]
    fn closing_adjusts_the_active_tab_and_never_empties_the_window() {
        let mut shell = shell();
        shell.open_document(KernelLabApp::default());
        shell.open_document(KernelLabApp::default());
        shell.activate(2);
        // Closing an earlier tab keeps the same document in front.
        assert!(shell.close(0).is_some());
        assert_eq!(shell.active_index(), 1);
        assert_eq!(shell.active_document().document_title(), "Document 3");
        // Closing the front tab moves to its neighbour.
        assert!(shell.close(1).is_some());
        assert_eq!(shell.active_index(), 0);
        assert_eq!(shell.titles(), vec!["Document 2".to_owned()]);
        // The last document stays.
        assert!(shell.close(0).is_none());
        assert_eq!(shell.document_count(), 1);
    }

    #[test]
    fn a_document_can_ask_the_shell_for_a_new_tab() {
        let mut app = KernelLabApp::default();
        app.request_from_shell(ShellRequest::NewDocument);
        assert_eq!(app.take_shell_requests(), vec![ShellRequest::NewDocument]);
        assert!(app.take_shell_requests().is_empty(), "requests drain once");
    }
}
