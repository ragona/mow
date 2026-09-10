//! Race instruments and a rival marker that remains useful on the far hemisphere.

use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Shape, Stroke, Vec2};
use glam::{Mat4, Vec3};
use lawn_core::{camera::CameraState, input::Action};

use super::{
    GARDEN_CORAL, GARDEN_CREAM, GARDEN_MUTED, GARDEN_PINE, GARDEN_RIVAL, LawnOrbitApp, RaceOutcome,
    UiCommand, action_label, display, format_time, garden_button, garden_card,
    garden_ui::{Icon, icon, keycap},
};

impl LawnOrbitApp {
    pub(super) fn draw_victory_hud(&self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        if self.victory_card_open {
            let race = self
                .run
                .race
                .as_ref()
                .expect("victory lap has race results");
            egui::Window::new("Victory lap")
                .id(egui::Id::new("victory-lap"))
                .anchor(Align2::RIGHT_CENTER, [-18.0, 0.0])
                .collapsible(false)
                .resizable(false)
                .title_bar(false)
                .frame(garden_card().inner_margin(16))
                .show(context, |ui| {
                    ui.set_width(270.0);
                    ui.label(
                        RichText::new("TURF RACE · YOU WIN")
                            .size(11.0)
                            .color(GARDEN_MUTED),
                    );
                    ui.label(RichText::new("The lawn is yours!").font(display(28.0)));
                    ui.label(
                        RichText::new("Your rival is out. Enjoy a little victory lap.")
                            .size(13.0)
                            .color(GARDEN_MUTED),
                    );
                    ui.add_space(8.0);
                    race_meter(ui, 270.0, race.player_coverage, race.rival_coverage);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format_time(
                                race.finished_seconds.unwrap_or(self.run.simulation_seconds),
                            ))
                            .font(display(22.0)),
                        );
                        ui.label(
                            RichText::new(format!("race time · {} bumps", race.bumps))
                                .size(11.0)
                                .color(GARDEN_MUTED),
                        );
                    });
                    ui.label(
                        RichText::new("Keep driving. The remaining grass is yours to mow.")
                            .size(12.0)
                            .color(GARDEN_MUTED),
                    );
                    ui.add_space(8.0);
                    if garden_button(ui, "Keep mowing", [270.0, 36.0], true).clicked() {
                        commands.push(UiCommand::KeepMowing);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Rematch").clicked() {
                            commands.push(UiCommand::Restart);
                        }
                        if ui.button("World editor").clicked() {
                            commands.push(UiCommand::ReturnToEditor);
                        }
                    });
                    if ui.button("New random planet").clicked() {
                        commands.push(UiCommand::Random);
                    }
                });
        } else {
            egui::Area::new("victory-status".into())
                .anchor(Align2::LEFT_BOTTOM, [18.0, -18.0])
                .show(context, |ui| {
                    garden_card().inner_margin(8).show(ui, |ui| {
                        if ui.button("Race won · View result").clicked() {
                            commands.push(UiCommand::ShowVictory);
                        }
                    });
                });
        }
    }

    pub(super) fn draw_race_hud(&self, context: &egui::Context, opacity: f32) {
        let Some(race) = &self.run.race else { return };
        egui::Area::new("race-score".into())
            .anchor(Align2::CENTER_TOP, [0.0, 18.0])
            .show(context, |ui| {
                ui.set_opacity(opacity);
                garden_card()
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(398.0);
                        race_meter(ui, 398.0, race.player_coverage, race.rival_coverage);
                    });
            });
        if self.scene_transition.is_some() {
            return;
        }
        self.draw_rival_marker(context);
        let stuck = self.run.vehicle.state.stuck_seconds >= 1.0;
        if self.run.simulation_seconds < 12.0 || stuck {
            let gamepad = self.input.last_device_label == "Gamepad";
            let recover = action_label(&self.profile.settings.controls, Action::Recover, gamepad);
            egui::Area::new("race-hint".into())
                .anchor(Align2::CENTER_BOTTOM, [0.0, -24.0])
                .show(context, |ui| {
                    garden_card().inner_margin(10).show(ui, |ui| {
                        if stuck {
                            ui.horizontal(|ui| {
                                keycap(ui, &recover);
                                ui.label("Hold to recover your mower");
                            });
                        } else {
                            ui.label(
                                RichText::new("Claim fresh grass. Bump your rival off their line.")
                                    .size(14.0),
                            );
                            ui.label(
                                RichText::new("Cut grass stays claimed — find a fresh route.")
                                    .size(11.0)
                                    .color(GARDEN_MUTED),
                            );
                        }
                    });
                });
        }
    }

    fn draw_rival_marker(&self, context: &egui::Context) {
        let Some(rival) = &self.run.rival else { return };
        let screen = context.content_rect();
        let camera = self.run.camera.state;
        let rival_position = rival.state.transform.position;
        let marker = rival_marker(
            camera,
            self.run.vehicle.state.transform.position,
            rival_position,
            screen,
        );
        let label = if marker.hidden {
            "Rival · far side"
        } else {
            "Rival"
        };
        egui::Area::new("rival-marker".into())
            .pivot(Align2::CENTER_CENTER)
            .fixed_pos(marker.position)
            .show(context, |ui| {
                egui::Frame::NONE
                    .fill(GARDEN_CREAM.gamma_multiply(0.94))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::symmetric(9, 5))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                            if marker.hidden {
                                icon(
                                    ui.painter(),
                                    rect.center(),
                                    15.0,
                                    Icon::Arrow(marker.angle),
                                    GARDEN_RIVAL,
                                );
                            } else {
                                ui.painter().add(Shape::convex_polygon(
                                    vec![
                                        rect.center_top(),
                                        rect.right_center(),
                                        rect.center_bottom(),
                                        rect.left_center(),
                                    ],
                                    GARDEN_RIVAL,
                                    Stroke::NONE,
                                ));
                            }
                            ui.label(RichText::new(label).size(11.0).color(GARDEN_PINE));
                        });
                    });
            });
    }

    pub(super) fn draw_race_results(&self, context: &egui::Context, commands: &mut Vec<UiCommand>) {
        let Some(race) = &self.run.race else { return };
        let Some(outcome) = race.outcome else { return };
        let (eyebrow, title, description) = match outcome {
            RaceOutcome::PlayerWon => (
                "TURF RACE · YOU WIN",
                "The lawn is yours!",
                "You claimed the bigger half of this little world.",
            ),
            RaceOutcome::RivalWon => (
                "TURF RACE · RIVAL WINS",
                "A rematch, perhaps?",
                "Your rival claimed the bigger half this time.",
            ),
            RaceOutcome::Draw => (
                "TURF RACE · A DRAW",
                "An evenly shared lawn.",
                "Every patch is claimed. This one is a draw.",
            ),
        };
        egui::Window::new("Turf race result")
            .id(egui::Id::new("race-results"))
            .anchor(Align2::RIGHT_CENTER, [-34.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(garden_card().inner_margin(22))
            .show(context, |ui| {
                ui.set_width(342.0);
                ui.label(RichText::new(eyebrow).size(11.0).color(GARDEN_MUTED));
                ui.label(RichText::new(title).font(display(32.0)));
                ui.label(RichText::new(description).size(13.0).color(GARDEN_MUTED));
                ui.add_space(12.0);
                race_meter(ui, 342.0, race.player_coverage, race.rival_coverage);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format_time(
                            race.finished_seconds.unwrap_or(self.run.simulation_seconds),
                        ))
                        .font(display(23.0)),
                    );
                    ui.label(
                        RichText::new(format!("elapsed · {} mower bumps", race.bumps))
                            .size(12.0)
                            .color(GARDEN_MUTED),
                    );
                });
                ui.label(
                    RichText::new(format!("Planet {}", self.run.planet.world_seed))
                        .size(11.0)
                        .color(GARDEN_MUTED),
                );
                ui.add_space(14.0);
                if garden_button(ui, "Rematch", [342.0, 40.0], true).clicked() {
                    commands.push(UiCommand::Restart);
                }
                ui.horizontal(|ui| {
                    if ui.button("World editor").clicked() {
                        commands.push(UiCommand::ReturnToEditor);
                    }
                    if ui.button("New random planet").clicked() {
                        commands.push(UiCommand::Random);
                    }
                });
            });
    }
}

fn race_meter(ui: &mut egui::Ui, width: f32, player: f64, rival: f64) {
    let player = player.clamp(0.0, 1.0) as f32;
    let rival = rival.clamp(0.0, 1.0) as f32;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Label,
            true,
            format!(
                "You {:.1} percent. Rival {:.1} percent. First past 50 percent wins.",
                player * 100.0,
                rival * 100.0
            ),
        )
    });
    let painter = ui.painter();
    painter.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        format!("YOU  {:.1}%", player * 100.0),
        FontId::proportional(13.0),
        GARDEN_PINE,
    );
    painter.text(
        rect.right_top(),
        Align2::RIGHT_TOP,
        format!("{:.1}%  RIVAL", rival * 100.0),
        FontId::proportional(13.0),
        GARDEN_PINE,
    );
    let track = Rect::from_min_size(rect.min + Vec2::new(0.0, 20.0), Vec2::new(width, 10.0));
    painter.rect_filled(track, 5, Color32::from_rgb(225, 228, 211));
    if player > 0.0 {
        painter.rect_filled(
            Rect::from_min_max(
                track.min,
                egui::pos2(track.left() + width * player, track.bottom()),
            ),
            5,
            GARDEN_CORAL,
        );
    }
    if rival > 0.0 {
        painter.rect_filled(
            Rect::from_min_max(
                egui::pos2(track.right() - width * rival.min(1.0 - player), track.top()),
                track.max,
            ),
            5,
            GARDEN_RIVAL,
        );
    }
    painter.line_segment(
        [
            track.center_top() - Vec2::new(0.0, 2.0),
            track.center_bottom() + Vec2::new(0.0, 2.0),
        ],
        Stroke::new(1.5, GARDEN_PINE),
    );
    // Team marks remain visible before either mower has claimed its first cell.
    painter.circle_filled(track.left_center(), 3.0, GARDEN_CORAL);
    painter.rect_filled(
        Rect::from_center_size(track.right_center(), Vec2::splat(6.0)),
        1,
        GARDEN_RIVAL,
    );
}

#[derive(Clone, Copy, Debug)]
struct RivalMarker {
    position: egui::Pos2,
    angle: f32,
    hidden: bool,
}

fn rival_marker(camera: CameraState, player: Vec3, rival: Vec3, screen: Rect) -> RivalMarker {
    let view = (camera.target - camera.position).normalize_or(Vec3::NEG_Z);
    let right = view.cross(camera.up).normalize_or(Vec3::X);
    let up = right.cross(view);
    let projection = Mat4::perspective_rh(
        camera.field_of_view_degrees.to_radians(),
        screen.aspect_ratio(),
        0.1,
        500.0,
    ) * Mat4::look_at_rh(camera.position, camera.target, camera.up);
    let clip = projection * rival.extend(1.0);
    let facing_camera = rival
        .normalize_or(Vec3::Y)
        .dot((camera.position - rival).normalize_or(Vec3::Y))
        > 0.10;
    let projected = if clip.w > 0.0 {
        clip.truncate() / clip.w
    } else {
        Vec3::splat(2.0)
    };
    let hidden = !facing_camera || projected.x.abs() > 0.94 || projected.y.abs() > 0.92;
    let tangent = rival
        .normalize_or(Vec3::Y)
        .reject_from_normalized(player.normalize_or(Vec3::Y));
    let direction = Vec2::new(tangent.dot(right), -tangent.dot(up)).normalized();
    let direction = if direction.length_sq() > 0.1 {
        direction
    } else {
        Vec2::Y
    };
    let position = if hidden {
        screen.center() + direction * screen.height() * 0.39
    } else {
        egui::pos2(
            screen.center().x + projected.x * screen.width() * 0.5,
            screen.center().y - projected.y * screen.height() * 0.5 - 28.0,
        )
    };
    RivalMarker {
        position: egui::pos2(
            position
                .x
                .clamp(screen.left() + 75.0, screen.right() - 75.0),
            position
                .y
                .clamp(screen.top() + 152.0, screen.bottom() - 95.0),
        ),
        angle: direction.x.atan2(-direction.y),
        hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rival_marker_distinguishes_visible_and_hidden_hemispheres() {
        let camera = CameraState {
            position: Vec3::Z * 26.0,
            target: Vec3::Z * 12.0,
            up: Vec3::Y,
            field_of_view_degrees: 90.0,
        };
        for size in [Vec2::new(960.0, 540.0), Vec2::new(1280.0, 720.0)] {
            let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
            let visible = rival_marker(camera, Vec3::Z * 15.0, Vec3::new(2.0, 0.0, 15.0), screen);
            assert!(!visible.hidden);
            let hidden = rival_marker(camera, Vec3::Z * 15.0, Vec3::NEG_Z * 15.0, screen);
            assert!(hidden.hidden);
            assert!(hidden.position.is_finite());
            assert!(screen.contains(hidden.position));
            assert!(hidden.position.y >= 152.0);
        }
    }

    #[test]
    fn hidden_rival_direction_tracks_camera_roll() {
        let mut camera = CameraState {
            position: Vec3::Z * 26.0,
            target: Vec3::Z * 12.0,
            up: Vec3::Y,
            field_of_view_degrees: 90.0,
        };
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(960.0, 540.0));
        let player = Vec3::Z * 15.0;
        let rival = Vec3::new(12.0, 0.0, -9.0);
        let normal = rival_marker(camera, player, rival, screen);
        camera.up = Vec3::NEG_Y;
        let rolled = rival_marker(camera, player, rival, screen);
        assert!(normal.hidden && rolled.hidden);
        assert!(normal.position.x > screen.center().x);
        assert!(rolled.position.x < screen.center().x);
        assert!((normal.angle.abs() - std::f32::consts::FRAC_PI_2).abs() < 0.001);
        assert!((normal.angle + rolled.angle).abs() < 0.001);
    }
}
