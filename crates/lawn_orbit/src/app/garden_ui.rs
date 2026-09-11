//! Small vector illustrations and instruments in the garden's visual language.

use egui::{
    Align2, Color32, FontFamily, FontId, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, Vec2,
};
use lawn_core::input::{Action, Binding, ControlMap};

use super::{GARDEN_CORAL, GARDEN_CREAM, GARDEN_MUTED, GARDEN_PINE, GARDEN_SAGE};

pub(super) fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("mow-display".into()))
}

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Leaf,
    Boost,
    Pause,
    Star,
    Arrow(f32),
    Rotate(bool),
}

pub(super) fn icon(painter: &egui::Painter, center: Pos2, size: f32, kind: Icon, color: Color32) {
    let point = |x: f32, y: f32| center + Vec2::new(x, y) * size;
    let stroke = Stroke::new(1.7, color);
    match kind {
        Icon::Leaf => {
            painter.add(Shape::convex_polygon(
                vec![
                    point(-0.35, 0.32),
                    point(-0.40, -0.08),
                    point(-0.18, -0.36),
                    point(0.40, -0.40),
                    point(0.33, 0.17),
                    point(0.0, 0.38),
                ],
                color,
                Stroke::NONE,
            ));
            painter.line_segment(
                [point(-0.38, 0.43), point(0.17, -0.16)],
                Stroke::new(1.1, GARDEN_CREAM),
            );
        }
        Icon::Boost => {
            painter.add(Shape::convex_polygon(
                vec![point(0.08, -0.47), point(-0.34, 0.05), point(0.06, 0.05)],
                color,
                Stroke::NONE,
            ));
            painter.add(Shape::convex_polygon(
                vec![point(0.35, -0.10), point(-0.14, 0.47), point(-0.04, -0.10)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Pause => {
            for x in [-0.15, 0.15] {
                painter.line_segment([point(x, -0.27), point(x, 0.27)], Stroke::new(2.5, color));
            }
        }
        Icon::Star => {
            let points: Vec<_> = (0..10)
                .map(|i| {
                    let angle = i as f32 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
                    let radius = if i % 2 == 0 { 0.48 } else { 0.22 };
                    point(angle.cos() * radius, angle.sin() * radius)
                })
                .collect();
            // An outlined star reads cleanly at both instrument and postcard sizes.
            painter.add(Shape::closed_line(points, stroke));
        }
        Icon::Arrow(angle) => {
            let rotate = |x: f32, y: f32| {
                point(
                    x * angle.cos() - y * angle.sin(),
                    x * angle.sin() + y * angle.cos(),
                )
            };
            painter.line_segment([rotate(0.0, 0.35), rotate(0.0, -0.35)], stroke);
            painter.add(Shape::line(
                vec![
                    rotate(-0.25, -0.05),
                    rotate(0.0, -0.35),
                    rotate(0.25, -0.05),
                ],
                stroke,
            ));
        }
        Icon::Rotate(clockwise) => {
            let sign = if clockwise { 1.0 } else { -1.0 };
            let points: Vec<_> = (0..=20)
                .map(|i| {
                    let angle =
                        -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI * 1.5 / 20.0;
                    point(angle.cos() * 0.32 * sign, angle.sin() * 0.32)
                })
                .collect();
            painter.add(Shape::line(points, stroke));
            painter.add(Shape::line(
                vec![
                    point(-0.43 * sign, 0.16),
                    point(-0.32 * sign, 0.0),
                    point(-0.17 * sign, 0.11),
                ],
                stroke,
            ));
        }
    }
}

pub(super) fn icon_button(ui: &mut egui::Ui, kind: Icon, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(25.0), Sense::click());
    let style = ui.style().interact(&response);
    ui.painter().rect(
        rect,
        6,
        style.weak_bg_fill,
        style.bg_stroke,
        egui::StrokeKind::Inside,
    );
    icon(ui.painter(), rect.center(), 19.0, kind, GARDEN_PINE);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response.on_hover_text(label)
}

/// The main play action: a raised coral face and a little orbital play badge.
/// All decoration shares one response, so pointer and keyboard activation agree.
pub(super) fn mow_button(ui: &mut egui::Ui, label: &str, size: [f32; 2]) -> Response {
    let (rect, response) = ui.allocate_exact_size(size.into(), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let pressed = response.is_pointer_button_down_on();
    let hovered = response.hovered();
    let painter = ui.painter();
    let radius = 17;
    let base = Color32::from_rgb(182, 91, 63);
    let face = Rect::from_min_max(
        rect.min + Vec2::new(0.0, if pressed { 4.0 } else { 0.0 }),
        rect.max - Vec2::new(0.0, if pressed { 1.0 } else { 5.0 }),
    );
    painter.rect_filled(rect, radius, base);
    painter.rect_filled(
        face,
        radius,
        if pressed {
            Color32::from_rgb(237, 134, 96)
        } else if hovered {
            Color32::from_rgb(255, 175, 129)
        } else {
            GARDEN_CORAL
        },
    );
    painter.rect_stroke(
        face.shrink(1.0),
        radius - 1,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 249, 231, 100)),
        egui::StrokeKind::Inside,
    );
    if response.has_focus() || hovered {
        painter.rect_stroke(
            rect.expand(3.0),
            radius + 3,
            Stroke::new(2.0, GARDEN_PINE),
            egui::StrokeKind::Outside,
        );
    }

    let center = face.center();
    let compact = size[1] < 80.0;
    painter.text(
        Pos2::new(face.left() + 20.0, center.y - 10.0),
        Align2::LEFT_CENTER,
        label,
        display(if compact { 28.0 } else { 38.0 }),
        GARDEN_PINE,
    );
    painter.text(
        Pos2::new(
            face.left() + 21.0,
            center.y + if compact { 16.0 } else { 21.0 },
        ),
        Align2::LEFT_CENTER,
        "Race a fresh planet",
        FontId::proportional(11.0),
        GARDEN_PINE,
    );

    let badge = Pos2::new(face.right() - 44.0, center.y);
    let orbit: Vec<_> = (0..=48)
        .map(|step| {
            let angle = step as f32 / 48.0 * std::f32::consts::TAU;
            let offset = Vec2::new(angle.cos() * 33.0, angle.sin() * 17.0);
            badge + egui::emath::Rot2::from_angle(-0.55) * offset
        })
        .collect();
    painter.add(Shape::line(
        orbit,
        Stroke::new(1.5, GARDEN_PINE.gamma_multiply(0.35)),
    ));
    painter.circle_filled(badge, 24.0, GARDEN_PINE);
    painter.circle_stroke(
        badge,
        20.5,
        Stroke::new(1.0, GARDEN_SAGE.gamma_multiply(0.35)),
    );
    painter.add(Shape::convex_polygon(
        vec![
            badge + Vec2::new(-5.0, -9.0),
            badge + Vec2::new(9.0, 0.0),
            badge + Vec2::new(-5.0, 9.0),
        ],
        GARDEN_CREAM,
        Stroke::NONE,
    ));
    icon(
        painter,
        badge + Vec2::new(20.0, -22.0),
        12.0,
        Icon::Leaf,
        GARDEN_PINE,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(super) fn preset_card(ui: &mut egui::Ui, label: &str, selected: bool, peaks: u8) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(94.0, 86.0), Sense::click());
    let painter = ui.painter();
    let active = selected || response.hovered() || response.has_focus();
    painter.rect_filled(
        rect,
        11,
        if selected {
            GARDEN_SAGE
        } else {
            Color32::from_rgb(246, 240, 222)
        },
    );
    painter.rect_stroke(
        rect,
        11,
        Stroke::new(
            if active { 1.5 } else { 1.0 },
            if active {
                GARDEN_PINE
            } else {
                Color32::from_rgb(227, 224, 204)
            },
        ),
        egui::StrokeKind::Inside,
    );
    let center = rect.center_top() + Vec2::new(0.0, 31.0);
    painter.circle_filled(
        center + Vec2::new(1.0, 2.5),
        20.0,
        Color32::from_black_alpha(15),
    );
    painter.circle_filled(center, 20.0, Color32::from_rgb(99, 148, 85));
    painter.circle_filled(
        center - Vec2::splat(4.0),
        14.0,
        Color32::from_rgb(147, 178, 106),
    );
    for i in 0..peaks {
        let x = (f32::from(i) - f32::from(peaks - 1) * 0.5) * 10.0;
        let height = 12.0 + f32::from((i + peaks) % 3) * 3.0;
        painter.add(Shape::convex_polygon(
            vec![
                center + Vec2::new(x - 8.0, 6.0),
                center + Vec2::new(x, -height),
                center + Vec2::new(x + 9.0, 6.0),
            ],
            Color32::from_rgb(131, 143, 178),
            Stroke::NONE,
        ));
        painter.add(Shape::convex_polygon(
            vec![
                center + Vec2::new(x, -height),
                center + Vec2::new(x + 9.0, 6.0),
                center + Vec2::new(x + 1.0, 4.0),
            ],
            Color32::from_rgb(101, 111, 154),
            Stroke::NONE,
        ));
    }
    painter.text(
        rect.center_bottom() - Vec2::new(0.0, 16.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        GARDEN_PINE,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    response
}

pub(super) fn coverage_dial(ui: &mut egui::Ui, percent: f32, accent: f32) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(164.0, 119.0), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Label,
            true,
            format!("{percent:.1}% of your planet freshly mown"),
        )
    });
    let center = rect.center_top() + Vec2::new(0.0, 57.0);
    let painter = ui.painter();
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |fraction: f32, radius: f32| -> Vec<Pos2> {
        let steps = (96.0 * fraction).ceil().max(1.0) as usize;
        (0..=steps)
            .map(|i| {
                let angle = start + sweep * fraction * i as f32 / steps as f32;
                center + Vec2::angled(angle) * radius
            })
            .collect()
    };
    painter.add(Shape::line(
        arc(1.0, 48.0),
        Stroke::new(5.0, Color32::from_rgb(227, 229, 205)),
    ));
    let fraction = (percent / 100.0).clamp(0.0, 1.0);
    if fraction > 0.0 {
        painter.add(Shape::line(
            arc(fraction, 48.0),
            Stroke::new(5.0 + accent * 1.0, GARDEN_PINE),
        ));
        let end = start + sweep * fraction;
        painter.circle_filled(center + Vec2::angled(end) * 48.0, 4.0, GARDEN_CORAL);
    }
    icon(
        painter,
        center - Vec2::new(0.0, 22.0),
        13.0,
        Icon::Leaf,
        GARDEN_MUTED,
    );
    painter.text(
        center + Vec2::new(0.0, 3.0),
        Align2::CENTER_CENTER,
        format!("{percent:.1}%"),
        display(29.0),
        GARDEN_PINE,
    );
    painter.text(
        center + Vec2::new(0.0, 27.0),
        Align2::CENTER_CENTER,
        "FRESHLY MOWN",
        FontId::proportional(9.0),
        GARDEN_MUTED,
    );
    painter.text(
        rect.center_bottom() - Vec2::new(0.0, 5.0),
        Align2::CENTER_CENTER,
        "A little tidier.",
        display(17.0),
        GARDEN_PINE,
    );
}

pub(super) fn boost_meter(ui: &mut egui::Ui, charge: f32, active: bool, ready_flash: f32) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(164.0, 26.0), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Label,
            true,
            format!("Boost charge: {:.0}%", charge * 100.0),
        )
    });
    let painter = ui.painter();
    icon(
        painter,
        rect.left_center() + Vec2::new(9.0, 0.0),
        16.0,
        Icon::Boost,
        GARDEN_PINE,
    );
    let start = rect.min + Vec2::new(24.0, 6.0);
    for index in 0..12 {
        let segment = Rect::from_min_size(
            start + Vec2::new(index as f32 * 9.0, 0.0),
            Vec2::new(6.0, 14.0),
        );
        let filled = charge > index as f32 / 12.0;
        let color = if filled {
            if active { GARDEN_PINE } else { GARDEN_CORAL }
        } else {
            Color32::from_rgb(231, 228, 209)
        };
        painter.rect_filled(segment, 2, color);
    }
    if ready_flash > 0.0 {
        painter.rect_stroke(
            rect.expand(1.0),
            6,
            Stroke::new(1.5, GARDEN_CORAL.gamma_multiply(ready_flash)),
            egui::StrokeKind::Inside,
        );
    }
}

pub(super) fn action_label(controls: &ControlMap, action: Action, gamepad: bool) -> String {
    controls
        .bindings
        .get(&action)
        .and_then(|bindings| {
            bindings.iter().find_map(|binding| match binding {
                Binding::Key(key) if !gamepad => Some(
                    match key.as_str() {
                        "ShiftLeft" | "ShiftRight" => "Shift",
                        "ControlLeft" | "ControlRight" => "Ctrl",
                        "AltLeft" | "AltRight" => "Alt",
                        "SuperLeft" | "SuperRight" => "Cmd",
                        "ArrowLeft" => "←",
                        "ArrowRight" => "→",
                        "ArrowUp" => "↑",
                        "ArrowDown" => "↓",
                        "Escape" => "Esc",
                        "Space" => "Space",
                        _ => key
                            .strip_prefix("Key")
                            .or_else(|| key.strip_prefix("Digit"))
                            .unwrap_or(key),
                    }
                    .to_owned(),
                ),
                Binding::GamepadButton(button) if gamepad => Some(
                    match button.as_str() {
                        "South" => "A / ×",
                        "East" => "B / ○",
                        "West" => "X / □",
                        "North" => "Y / △",
                        "LeftTrigger" => "LB / L1",
                        "RightTrigger" => "RB / R1",
                        "LeftTrigger2" => "LT / L2",
                        "RightTrigger2" => "RT / R2",
                        "Start" => "Menu",
                        _ => button,
                    }
                    .to_owned(),
                ),
                Binding::MouseButton(button) if !gamepad => Some(format!("Mouse {button}")),
                Binding::GamepadAxis { axis, direction } if gamepad => {
                    let suffix = if *direction >= 0 { "+" } else { "−" };
                    Some(format!(
                        "{} {suffix}",
                        match axis.as_str() {
                            "LeftStickX" => "LS X",
                            "LeftStickY" => "LS Y",
                            "RightStickX" => "RS X",
                            "RightStickY" => "RS Y",
                            "LeftZ" => "LT / L2",
                            "RightZ" => "RT / R2",
                            _ => axis,
                        }
                    ))
                }
                _ => None,
            })
        })
        .unwrap_or_else(|| "—".into())
}

pub(super) fn keycap(ui: &mut egui::Ui, text: &str) {
    egui::Frame::NONE
        .fill(Color32::from_rgb(239, 235, 215))
        .stroke(Stroke::new(1.0, Color32::from_rgb(217, 219, 196)))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(5, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.0).color(GARDEN_PINE));
        });
}
