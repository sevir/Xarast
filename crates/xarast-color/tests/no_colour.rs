//! "No colour" as a value of its own (XARA-T-0204).

use xarast_color::{Colour, ColourDef, ColourTable, ColourValue, FillEffect, Stop};

fn clear() -> Colour {
    Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0))
}

#[test]
fn no_colour_is_not_transparent_black_but_resolves_to_it() {
    let t = ColourTable::new();
    assert!(Colour::None.is_none());
    assert!(!clear().is_none());
    assert_ne!(Colour::None, clear());
    assert_eq!(Colour::None.resolve(&t), Colour::NONE_VALUE);
    assert_eq!(Colour::None.resolve(&t).to_rgba8().a, 0);
    assert_eq!(Colour::None.resolve(&t).to_rgba8_packed().a, 0);
}

#[test]
fn to_direct_keeps_no_colour_and_bakes_references() {
    let mut t = ColourTable::new();
    let id = t.insert(ColourDef::normal(ColourValue::rgbt(0.2, 0.4, 0.6, 0.0)).named("Sea"));
    assert_eq!(Colour::None.to_direct(&t), Colour::None);
    assert_eq!(
        Colour::Indexed { id, tint: None }.to_direct(&t),
        Colour::Direct(ColourValue::rgbt(0.2, 0.4, 0.6, 0.0))
    );
}

#[test]
fn mixing_follows_the_original() {
    let red = Colour::Direct(ColourValue::rgbt(1.0, 0.0, 0.0, 0.0));
    // Both ends "no colour": "no colour".
    assert_eq!(
        Colour::None.lerp(&Colour::None, 0.5, FillEffect::Fade),
        Colour::None
    );
    // One end: it fades out, exactly as transparent black did before.
    for t in [0.0, 0.25, 0.5, 1.0] {
        assert_eq!(
            Colour::None.lerp(&red, t, FillEffect::Fade),
            clear().lerp(&red, t, FillEffect::Fade),
        );
    }
}

/// A palette entry holds a `ColourValue`, never a `Colour`: no palette
/// edit can produce "no colour" (the change journal relies on it).
#[test]
fn a_palette_entry_cannot_be_no_colour() {
    let mut t = ColourTable::new();
    let id = t.insert(ColourDef::normal(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0)));
    let c = Colour::Indexed { id, tint: None };
    assert!(!c.is_none());
    assert_eq!(c.resolve(&t).to_rgba8().a, 0);
}
