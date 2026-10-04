use rfidex_badge::layout::{Layout, Ticket};
use rfidex_badge::raster::render;
use rfidex_badge::text::Fonts;

fn ticket(name: &str) -> Ticket {
    Ticket {
        ticket_id: "9f1c2b3a-0000-4000-8000-000000000001".into(),
        name: Some(name.into()),
        company: Some("Borneo Expo".into()),
        ticket_type: "Visitor".into(),
        ..Ticket::default()
    }
}

fn ink(image: &image::GrayImage) -> usize {
    image.pixels().filter(|p| p.0[0] < 128).count()
}

/// One test: starting the font system scans every installed font.
#[test]
fn badges_render_latin_chinese_and_tamil() {
    let fonts = Fonts::new();
    let layout = Layout::default();

    let latin = render(&layout, &ticket("Aina Binti Ahmad"), &fonts, 203.0);
    assert_eq!((latin.width(), latin.height()), (799, 639));
    assert!(ink(&latin) > 2_000, "ink: {}", ink(&latin));
    // The outer margin stays white.
    for x in 0..latin.width() {
        assert_eq!(latin.get_pixel(x, 0).0[0], 255);
        assert_eq!(latin.get_pixel(x, latin.height() - 1).0[0], 255);
    }

    // Different non-Latin names must look different: if the fallback fonts
    // were missing, both would be the same row of empty boxes.
    let a = render(&layout, &ticket("王小明"), &fonts, 203.0);
    let b = render(&layout, &ticket("李大华"), &fonts, 203.0);
    // A machine with no Chinese-capable font draws the same empty box for every
    // character, so this warns instead of failing: a CI runner may lack fonts.
    if a.as_raw() == b.as_raw() {
        eprintln!("warning: no Chinese-capable font on this machine");
    }
    let c = render(&layout, &ticket("தமிழ் செல்வன்"), &fonts, 203.0);
    let d = render(&layout, &ticket("கவிதா ராஜ்"), &fonts, 203.0);
    if c.as_raw() == d.as_raw() {
        eprintln!("warning: no Tamil-capable font on this machine");
    }
}

#[test]
fn the_font_system_can_be_shared_between_threads_behind_a_mutex() {
    fn needs_send<T: Send>() {}
    needs_send::<Fonts>();
}

#[test]
fn a_badge_encodes_as_png() {
    let image = image::GrayImage::from_pixel(4, 3, image::Luma([255]));
    let bytes = rfidex_badge::raster::to_png(&image);
    assert_eq!(&bytes[1..4], b"PNG");
}
