//! IBM Plex Sans for Direct2D text, the face the settings window draws with, so native surfaces
//! (the tray menu) read as the same app. Loaded from the bytes built into the binary once per
//! thread; Segoe UI stands in should DirectWrite refuse them.

use std::cell::OnceCell;

use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{HSTRING, Interface, w};

pub const SANS_REGULAR: &[u8] = include_bytes!("../../../../assets/fonts/IBMPlexSans-Regular.ttf");
pub const SANS_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/IBMPlexSans-SemiBold.ttf");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weight {
    Regular,
    SemiBold,
}

impl Weight {
    fn dwrite(self) -> DWRITE_FONT_WEIGHT {
        match self {
            Weight::Regular => DWRITE_FONT_WEIGHT_REGULAR,
            Weight::SemiBold => DWRITE_FONT_WEIGHT_SEMI_BOLD,
        }
    }
}

thread_local! {
    static PLEX: OnceCell<Option<IDWriteFontCollection1>> = const { OnceCell::new() };
}

/// Plex Sans as a font collection of its own (typographic family "IBM Plex Sans").
fn load(dwrite: &IDWriteFactory) -> Result<IDWriteFontCollection1, windows::core::Error> {
    unsafe {
        let f6: IDWriteFactory6 = dwrite.cast()?;
        let loader = f6.CreateInMemoryFontFileLoader()?;
        f6.RegisterFontFileLoader(&loader)?;
        let builder = f6.CreateFontSetBuilder()?;
        for bytes in [SANS_REGULAR, SANS_SEMIBOLD] {
            // a non-null owner keeps DirectWrite from copying the data, which is 'static anyway
            let file = loader.CreateInMemoryFontFileReference(&f6, bytes.as_ptr() as _, bytes.len() as u32, &f6)?;
            IDWriteFontSetBuilder1::AddFontFile(&builder, &file)?;
        }
        let set = builder.CreateFontSet()?;
        f6.CreateFontCollectionFromFontSet(&set, DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC)?.cast()
    }
}

fn plex(dwrite: &IDWriteFactory) -> Option<IDWriteFontCollection1> {
    PLEX.with(|p| {
        p.get_or_init(|| {
            load(dwrite)
                .inspect_err(|e| glint_core::log::log(&format!("plex sans didn't load, using segoe ui: {e}")))
                .ok()
        })
        .clone()
    })
}

/// A single-line, vertically centred text format of Plex Sans at `size` physical pixels.
pub(super) fn format(dwrite: &IDWriteFactory, weight: Weight, size: f32) -> Result<IDWriteTextFormat, String> {
    unsafe {
        let make = |family: &HSTRING, collection: Option<&IDWriteFontCollection>| {
            dwrite.CreateTextFormat(
                family,
                collection,
                weight.dwrite(),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )
        };
        let plex = plex(dwrite);
        let f = match &plex {
            Some(c) => make(&HSTRING::from("IBM Plex Sans"), Some(&c.cast().map_err(|e| e.to_string())?)),
            None => make(&HSTRING::from("Segoe UI"), None),
        }
        .map_err(|e| format!("CreateTextFormat: {e}"))?;
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plex_sans_loads_with_both_weights() {
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.unwrap();
        let c = plex(&dwrite).expect("the built-in fonts load");
        let (mut index, mut exists) = (0, windows::core::BOOL(0));
        unsafe { c.FindFamilyName(w!("IBM Plex Sans"), &mut index, &mut exists) }.unwrap();
        assert!(exists.as_bool());
        let family = unsafe { c.GetFontFamily(index) }.unwrap();
        for weight in [Weight::Regular, Weight::SemiBold] {
            let font = unsafe {
                family.GetFirstMatchingFont(weight.dwrite(), DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
            }
            .unwrap();
            assert_eq!(unsafe { font.GetWeight() }, weight.dwrite());
        }
        // a second painter on the thread reuses the collection
        assert!(plex(&dwrite).is_some());
    }
}
