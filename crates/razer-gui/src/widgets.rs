use crate::theme;
use iced::alignment;
use iced::mouse;
use iced::widget::canvas::{self, Path, Stroke, stroke};
use iced::widget::{column, container, row, text};
use iced::{Color, Element, Fill, Point, Radians, Rectangle, Renderer, Theme};

pub fn gauge_fraction(value: f32, min: f32, max: f32) -> f32 {
    if min >= max {
        return 0.0;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// Speedometer-style arc: 240 degrees of sweep starting at 150 degrees
/// (down-left) through 30 degrees (down-right), matching the approved mockups.
pub struct Gauge {
    pub fraction: Option<f32>,
    pub value: String,
    pub unit: String,
    pub warm: bool,
}

const SWEEP: f32 = 240.0_f32.to_radians();
const START: f32 = 150.0_f32.to_radians();

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn blend(from: Color, to: Color, t: f32) -> Color {
    Color::from_rgb(
        lerp(from.r, to.r, t),
        lerp(from.g, to.g, t),
        lerp(from.b, to.b, t),
    )
}

impl<M> canvas::Program<M> for Gauge {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = Point::new(frame.width() / 2.0, frame.height() * 0.58);
        let radius = (frame.width() / 2.0 - 10.0).min(frame.height() * 0.52);
        let track_width = radius * 0.16;

        let arc_path = |sweep_fraction: f32| {
            Path::new(|builder| {
                builder.arc(canvas::path::Arc {
                    center,
                    radius,
                    start_angle: Radians(START),
                    end_angle: Radians(START + SWEEP * sweep_fraction),
                })
            })
        };

        let track_stroke = Stroke {
            width: track_width,
            style: stroke::Style::Solid(theme::BORDER),
            line_cap: stroke::LineCap::Round,
            ..Stroke::default()
        };
        frame.stroke(&arc_path(1.0), track_stroke);

        let (value_color, value_text_color) = match self.fraction {
            Some(fraction) => {
                let color = if self.warm {
                    blend(theme::ACCENT, theme::WARM, fraction)
                } else {
                    theme::ACCENT
                };
                let path = arc_path(fraction.clamp(0.0, 1.0));
                // Wide translucent understroke reads as a neon glow.
                frame.stroke(
                    &path,
                    Stroke {
                        width: track_width * 2.4,
                        style: stroke::Style::Solid(Color { a: 0.18, ..color }),
                        line_cap: stroke::LineCap::Round,
                        ..Stroke::default()
                    },
                );
                frame.stroke(
                    &path,
                    Stroke {
                        width: track_width,
                        style: stroke::Style::Solid(color),
                        line_cap: stroke::LineCap::Round,
                        ..Stroke::default()
                    },
                );
                (color, theme::TEXT_BRIGHT)
            }
            None => (theme::MUTED, theme::MUTED),
        };

        frame.fill_text(canvas::Text {
            content: if self.fraction.is_some() {
                self.value.clone()
            } else {
                "—".to_string()
            },
            position: Point::new(center.x, center.y - radius * 0.18),
            color: value_text_color,
            size: (radius * 0.42).into(),
            align_x: text::Alignment::Center,
            align_y: alignment::Vertical::Center,
            ..canvas::Text::default()
        });
        frame.fill_text(canvas::Text {
            content: self.unit.clone(),
            position: Point::new(center.x, center.y + radius * 0.22),
            color: if self.fraction.is_some() {
                theme::MUTED
            } else {
                value_color
            },
            size: (radius * 0.16).into(),
            align_x: text::Alignment::Center,
            align_y: alignment::Vertical::Center,
            ..canvas::Text::default()
        });

        vec![frame.into_geometry()]
    }
}

pub fn gauge_card<'a, M: 'a>(label: &'a str, gauge: Gauge) -> Element<'a, M> {
    let dimmed = gauge.fraction.is_none();
    let label_text = text(label.to_uppercase()).size(11).color(theme::MUTED);
    let card = container(
        column![
            label_text,
            canvas::Canvas::new(gauge).width(Fill).height(120)
        ]
        .spacing(6)
        .align_x(iced::Center),
    )
    .style(theme::card)
    .padding(12)
    .width(Fill);
    if dimmed {
        // Keep the card in place; absence reads as a dimmed em-dash state.
        card.style(|t: &Theme| iced::widget::container::Style {
            text_color: Some(theme::MUTED),
            ..theme::card(t)
        })
        .into()
    } else {
        card.into()
    }
}

pub fn section<'a, M: 'a>(title: Option<&'a str>, rows: Vec<Element<'a, M>>) -> Element<'a, M> {
    let mut content = column![].spacing(10);
    if let Some(title) = title {
        content = content.push(text(title.to_uppercase()).size(11).color(theme::MUTED));
    }
    for r in rows {
        content = content.push(r);
    }
    container(content)
        .style(theme::card)
        .padding(14)
        .width(Fill)
        .into()
}

pub fn setting_row<'a, M: 'a>(
    title: &'a str,
    subtitle: &'a str,
    control: Element<'a, M>,
) -> Element<'a, M> {
    let labels = column![
        text(title).size(14).color(theme::TEXT_BRIGHT),
        text(subtitle).size(11).color(theme::MUTED),
    ]
    .spacing(2)
    .width(Fill);
    row![labels, container(control).align_y(iced::Center)]
        .spacing(12)
        .align_y(iced::Center)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_fraction_clamps_and_maps_linearly() {
        assert_eq!(gauge_fraction(50.0, 0.0, 100.0), 0.5);
        assert_eq!(gauge_fraction(-10.0, 0.0, 100.0), 0.0);
        assert_eq!(gauge_fraction(200.0, 0.0, 100.0), 1.0);
        // fan gauge case: rpm within a mode-specific band
        assert_eq!(gauge_fraction(4300.0, 3300.0, 5300.0), 0.5);
        // degenerate range must not divide by zero
        assert_eq!(gauge_fraction(1.0, 5.0, 5.0), 0.0);
    }
}
