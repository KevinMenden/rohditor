use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
    Menu,
    Undo,
    Redo,
    Reset,
    Diagnostics,
    BeforeAfter,
    File,
    Photo,
}

pub(crate) fn paint(painter: &egui::Painter, rect: egui::Rect, icon: Icon, color: egui::Color32) {
    let rect = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(15.0));
    let stroke = egui::Stroke::new(1.5_f32, color);
    match icon {
        Icon::Menu => {
            for y in [rect.top() + 3.0, rect.center().y, rect.bottom() - 3.0] {
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + 2.0, y),
                        egui::pos2(rect.right() - 2.0, y),
                    ],
                    stroke,
                );
            }
        }
        Icon::Undo | Icon::Redo => {
            let direction = if icon == Icon::Undo { -1.0 } else { 1.0 };
            let center = rect.center();
            painter.circle_stroke(center, 5.0, stroke);
            let tip = egui::pos2(center.x + direction * 6.5, center.y - 1.5);
            painter.line_segment([tip, tip + egui::vec2(-direction * 3.5, -3.0)], stroke);
            painter.line_segment([tip, tip + egui::vec2(-direction * 3.5, 3.0)], stroke);
        }
        Icon::Reset => {
            painter.circle_stroke(rect.center(), 5.5, stroke);
            let tip = rect.center() + egui::vec2(-6.5, -2.0);
            painter.line_segment([tip, tip + egui::vec2(3.5, -2.5)], stroke);
            painter.line_segment([tip, tip + egui::vec2(0.5, 4.0)], stroke);
        }
        Icon::Diagnostics => {
            for (index, y) in [rect.top() + 3.0, rect.center().y, rect.bottom() - 3.0]
                .into_iter()
                .enumerate()
            {
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + 1.5, y),
                        egui::pos2(rect.right() - 1.5, y),
                    ],
                    stroke,
                );
                let x = if index == 1 {
                    rect.left() + 5.0
                } else {
                    rect.right() - 5.0
                };
                painter.circle_filled(egui::pos2(x, y), 1.8, color);
            }
        }
        Icon::BeforeAfter => {
            painter.circle_stroke(rect.center(), 6.0, stroke);
            painter.line_segment(
                [
                    egui::pos2(rect.center().x, rect.top() + 1.5),
                    egui::pos2(rect.center().x, rect.bottom() - 1.5),
                ],
                stroke,
            );
            let half = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 2.0, rect.top() + 2.0),
                egui::pos2(rect.center().x, rect.bottom() - 2.0),
            );
            painter.rect_filled(half, 3.0, color.gamma_multiply(0.35));
        }
        Icon::File => {
            let page = rect.shrink2(egui::vec2(3.0, 1.0));
            painter.rect_stroke(page, 1.5, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    egui::pos2(page.left() + 3.0, page.top() + 5.0),
                    egui::pos2(page.right() - 3.0, page.top() + 5.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(page.left() + 3.0, page.top() + 8.5),
                    egui::pos2(page.right() - 3.0, page.top() + 8.5),
                ],
                stroke,
            );
        }
        Icon::Photo => {
            let frame = rect.shrink2(egui::vec2(2.0, 2.5));
            painter.rect_stroke(frame, 1.5, stroke, egui::StrokeKind::Inside);
            painter.circle_filled(
                egui::pos2(frame.left() + 4.0, frame.top() + 4.0),
                1.6,
                color,
            );
            let base = frame.bottom() - 1.5;
            painter.line_segment(
                [
                    egui::pos2(frame.left() + 1.5, base),
                    egui::pos2(frame.center().x - 1.0, base - 4.5),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(frame.center().x - 1.0, base - 4.5),
                    egui::pos2(frame.center().x + 2.5, base),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(frame.center().x + 1.0, base - 2.0),
                    egui::pos2(frame.center().x + 4.0, base - 5.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(frame.center().x + 4.0, base - 5.0),
                    egui::pos2(frame.right() - 1.5, base),
                ],
                stroke,
            );
        }
    }
}
