use rand::RngExt;
use serenity::all::Colour;

pub const BLURPLE: Colour = Colour::from_rgb(88, 101, 242);
pub const GREEN: Colour = Colour::from_rgb(87, 242, 135);
pub const YELLOW: Colour = Colour::from_rgb(254, 231, 92);
pub const RED: Colour = Colour::from_rgb(237, 66, 69);
pub const BLUE: Colour = Colour::from_rgb(0, 162, 255);
pub const GRAY: Colour = Colour::from_rgb(153, 170, 181);
pub const ORANGE: Colour = Colour::from_rgb(255, 165, 0);
pub const WHITE: Colour = Colour::from_rgb(255, 255, 255);
pub const BLACK: Colour = Colour::from_rgb(0, 0, 0);
pub const DARK_GRAY: Colour = Colour::from_rgb(43, 45, 49);
pub const LIGHT_GRAY: Colour = Colour::from_rgb(153, 170, 181);
pub const PURPLE: Colour = Colour::from_rgb(155, 89, 182);
pub const PINK: Colour = Colour::from_rgb(235, 69, 158);
pub const GOLD: Colour = Colour::from_rgb(241, 196, 15);
pub const TEAL: Colour = Colour::from_rgb(26, 188, 156);
pub const CYAN: Colour = Colour::from_rgb(0, 255, 255);
pub const MAGENTA: Colour = Colour::from_rgb(255, 0, 255);
pub const FUCHSIA: Colour = Colour::from_rgb(235, 69, 158);

const PALETTE: [Colour; 12] = [
    BLURPLE, GREEN, YELLOW, RED, BLUE, ORANGE, PURPLE, PINK, GOLD, TEAL, CYAN, MAGENTA,
];

/// Pick a random color from the palette.
pub fn random() -> Colour {
    let idx = rand::rng().random_range(0..PALETTE.len());
    PALETTE[idx]
}
