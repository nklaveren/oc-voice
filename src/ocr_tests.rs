//! Tests for the screen-reading helpers. Nothing here runs grim or tesseract:
//! what is worth pinning is the parsing and the geometry arithmetic, which is
//! where a silent mistake would send the grab to the wrong rectangle.

use super::*;

#[test]
fn a_region_survives_a_round_trip_through_its_own_spelling() {
    // The dump prints geometry that gets pasted straight into `ocr watch`.
    // If those two spellings disagree, the tool reads a different rectangle
    // than the one it told you to read.
    let r = Region {
        x: 494,
        y: 812,
        w: 260,
        h: 24,
    };
    assert_eq!(r.geometry(), "494,812 260x24");
    assert_eq!(Region::parse(&r.geometry()), Some(r));
    assert_eq!(
        Region::parse("  4,606 1528x830 "),
        Some(Region {
            x: 4,
            y: 606,
            w: 1528,
            h: 830
        })
    );
    assert_eq!(Region::parse("nonsense"), None);
    assert_eq!(Region::parse("4,606"), None);
}

#[test]
fn a_window_with_no_size_yields_no_region() {
    // hyprctl reports zero size for windows that are mapped but not yet laid
    // out. Grabbing 0x0 makes grim fail with a message about geometry rather
    // than about timing, which sends you looking in the wrong place.
    let mut win = WindowInfo::default();
    assert_eq!(Region::of_window(&win), None);
    win.size = [1528, 830];
    win.at = [4, 606];
    assert_eq!(
        Region::of_window(&win),
        Some(Region {
            x: 4,
            y: 606,
            w: 1528,
            h: 830
        })
    );
}

/// Real tesseract TSV, from the capture this module was built against.
const TSV: &str =
    "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
5\t1\t1\t1\t1\t1\t12\t4\t74\t16\t96.2\tPenubolu,
5\t1\t1\t1\t1\t2\t92\t4\t40\t16\t95.8\tVijay
5\t1\t1\t1\t1\t3\t140\t4\t30\t16\t89.1\t3:56
5\t1\t1\t1\t2\t1\t12\t28\t50\t16\t41.0\tnoise
5\t1\t1\t1\t2\t2\t70\t28\t20\t16\t93.0\ttext
5\t1\t1\t1\t1\t4\t0\t0\t0\t0\t-1\t   ";

#[test]
fn tsv_parsing_keeps_positions_and_drops_blanks() {
    let words = parse_tsv(TSV, 1.0);
    assert_eq!(words.len(), 5, "the whitespace-only row is not a word");
    assert_eq!(words[0].text, "Penubolu,");
    assert_eq!(words[0].left, 12);
    assert!((words[0].conf - 96.2).abs() < 0.01);
}

#[test]
fn positions_come_back_in_the_units_grim_speaks() {
    // The bug this pins, and it only shows on a scaled monitor: `grim -g`
    // takes logical pixels and writes physical ones, so on a 1.67x panel
    // every OCR coordinate came back magnified. Added to a logical window
    // origin it pointed at a rectangle outside the window entirely — a name
    // reported 1197 px down a window 830 px tall.
    let words = parse_tsv(TSV, 1.67);
    assert_eq!(
        words[0].left, 7,
        "12 physical px is 7 logical on a 1.67x panel"
    );
    assert_eq!(words[1].left, 55);
    // And an unscaled monitor is untouched.
    assert_eq!(parse_tsv(TSV, 1.0)[0].left, 12);
}

#[test]
fn png_width_is_read_from_the_header_or_refused() {
    // A truncated or non-PNG file must not silently yield a scale of 1.0
    // through a parsed-garbage width; it returns None and the caller falls
    // back deliberately.
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&2546u32.to_be_bytes());
    png.extend_from_slice(&1383u32.to_be_bytes());
    assert_eq!(png_width(&png), Some(2546));
    assert_eq!(png_width(&png[..20]), None, "truncated header");
    assert_eq!(png_width(b"not a png at all really"), None);
}

#[test]
fn lines_take_the_worst_confidence_not_the_average() {
    // One unreadable word in a name makes the whole name untrustworthy.
    // A mean would show 67 for a line holding a 41 — readable-looking, wrong.
    let lines = lines(&parse_tsv(TSV, 1.0));
    assert_eq!(lines.len(), 2, "two vertical positions, two lines");
    assert_eq!(lines[0].2, "Penubolu, Vijay 3:56");
    assert!((lines[0].3 - 89.1).abs() < 0.01, "worst of the three");
    assert!(
        (lines[1].3 - 41.0).abs() < 0.01,
        "the 41 must not be averaged away by the 93 beside it"
    );
}
