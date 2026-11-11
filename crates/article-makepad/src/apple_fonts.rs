//! Use the OS-provided PingFang faces on macOS and iOS, without shipping Apple fonts.
//!
//! Declare the system face in the script font family itself. Overriding only
//! the native loader cache is lost when Makepad reconciles script members on draw.

use std::{
    ffi::{CString, OsString, c_char, c_void},
    os::unix::ffi::OsStringExt,
    path::PathBuf,
    ptr,
    rc::Rc,
};
use makepad_widgets::{
    *,
    // JIT PORT: upstream renamed the fork's SharedBytes to HostBuf (FontData).
    makepad_draw::text::{font_face::FontFace, loader::{FontData as SharedBytes, FontDefinition}},
    makepad_platform::script::res::{CxScriptResource, CxScriptResourceData, CxScriptResourceGc},
};

const REGULAR: &str = "PingFangSC-Regular";
const SEMIBOLD: &str = "PingFangSC-Semibold";

pub fn install(vm: &mut ScriptVm) {
    // Resolve both weights before changing any families. Missing system assets
    // leave the existing cross-platform theme usable, including CJK and emoji.
    let faces = [REGULAR, SEMIBOLD].map(|name| system_face_resource(vm, name));
    let [Ok(regular), Ok(semibold)] = faces else {
        log!(
            "PingFang unavailable; keeping bundled fonts: {:?}",
            faces.map(|face| face.err())
        );
        return;
    };
    // widgets_mod selects the light/dark base before registering widgets.
    // Declare both bases so the chosen appearance inherits the same fonts.
    // A stylesheet's later explicit font choice (e.g. mono) still wins.
    let themes = [
        script_eval!(vm, { mod.themes.light }),
        script_eval!(vm, { mod.themes.dark }),
        script_eval!(vm, { mod.theme }),
    ];
    let mut seen = Vec::new();
    let family_proto = script_eval!(vm, { mod.text.FontFamily });
    let member_proto = script_eval!(vm, { mod.text.FontMember });
    for theme in themes {
        let Some(theme) = theme.as_object() else {
            continue;
        };
        if seen.contains(&theme) {
            continue;
        }
        seen.push(theme);
        let code = vm.bx.heap.value(theme, id!(font_code).into(), NoTrap);
        let code = TextStyle::script_from_value(vm, code);
        for (role, resource) in [
            (id!(font_label), regular),
            (id!(font_regular), regular),
            (id!(font_bold), semibold),
            (id!(font_italic), regular),
            (id!(font_bold_italic), semibold),
        ] {
            let previous = vm.bx.heap.value(theme, role.into(), NoTrap);
            let Some(previous_obj) = previous.as_object() else {
                continue;
            };
            if TextStyle::script_from_value(vm, previous).font_family == code.font_family {
                continue;
            }
            let previous_family = vm
                .bx
                .heap
                .value(previous_obj, id!(font_family).into(), NoTrap);
            let Some(previous_family) = previous_family.as_object() else {
                continue;
            };
            let family = vm.bx.heap.new_with_proto(family_proto);
            let primary = vm.bx.heap.new_with_proto(member_proto);
            vm.bx
                .heap
                .set_value(primary, id!(res).into(), resource, NoTrap);
            vm.bx
                .heap
                .vec_push(family, id_lut!(apple_system).into(), primary.into(), NoTrap);
            // Keep the declared fallback members, including their lazy-load flags.
            for i in 0..vm.bx.heap.vec_len(previous_family) {
                let kv = vm.bx.heap.vec_key_value(previous_family, i, NoTrap);
                if kv.key != id_lut!(apple_system).into() {
                    vm.bx.heap.vec_push(family, kv.key, kv.value, NoTrap);
                }
            }
            // Retain style metrics and keep the code family untouched.
            let style = vm.bx.heap.new_with_proto(previous);
            vm.bx
                .heap
                .set_value(style, id!(font_family).into(), family.into(), NoTrap);
            vm.bx
                .heap
                .set_value(theme, role.into(), style.into(), NoTrap);
        }
    }
    // PingFang has no italic face; prose uses the corresponding upright weight.
    log!(
        "Apple typography: PingFang SC Regular/Semibold declared in theme font families; bundled fallback retained"
    );
}

/// Whether both PingFang SC faces resolve to installed files on this machine.
/// Hosts without Apple's downloadable PingFang asset, such as GitHub's macOS 15
/// runners, keep the bundled fonts; tests that assert PingFang selection
/// skip there and say so instead of failing on the environment.
pub fn installed() -> bool {
    [REGULAR, SEMIBOLD]
        .iter()
        .all(|name| load_system_face(name).is_ok())
}

/// The installed PingFang SC Regular and Semibold faces as script font
/// resources, for VMs that build their own theme, such as Splash isolates.
pub fn pingfang(vm: &mut ScriptVm) -> Result<(ScriptValue, ScriptValue), String> {
    Ok((
        system_face_resource(vm, REGULAR)?,
        system_face_resource(vm, SEMIBOLD)?,
    ))
}

fn system_face_resource(vm: &mut ScriptVm, name: &str) -> Result<ScriptValue, String> {
    let path = format!("apple-system-font://{name}");
    let heap_key = vm.bx.heap.heap_key();
    if let Some(handle) = vm
        .cx()
        .script_data
        .resources
        .get_handle_by_abs_path(heap_key, &path)
    {
        return Ok(handle.into());
    }
    let data = if vm
        .cx()
        .script_data
        .resources
        .resources
        .borrow()
        .iter()
        .any(|r| r.abs_path == path)
    {
        None
    } else {
        let face = load_system_face(name)?;
        Some(Rc::new(face_at_index_zero(&face)?))
    };
    let resource_type = vm.handle_type(id_lut!(res));
    let resources = &vm.cx().script_data.resources;
    let gc = CxScriptResourceGc {
        resources: resources.resources.clone(),
        handles_by_abs_path: resources.handles_by_abs_path.clone(),
        handle: ScriptHandle::ZERO,
        heap_key,
    };
    let handle = vm.bx.heap.new_handle(resource_type, Box::new(gc));
    let resources = &vm.cx().script_data.resources;
    if !resources.attach_handle_for_path(heap_key, &path, handle) {
        resources.insert_resource(
            heap_key,
            CxScriptResource {
                abs_path: path,
                dependency_path: None,
                web_url: None,
                data: CxScriptResourceData::Loaded(data.expect("new system font resource")),
                handles: vec![(heap_key, handle)],
            },
        );
    }
    Ok(handle.into())
}

/// FontMember loads collection index zero. Point that TTC directory entry at
/// the requested installed face; font tables and their checksums stay intact.
/// This is an in-memory view of a local font, never a shipped or saved asset.
fn face_at_index_zero(face: &FontDefinition) -> Result<Vec<u8>, String> {
    let mut data = face.data.as_slice().to_vec();
    if data.starts_with(b"ttcf") {
        let count = data.get(8..12).ok_or("truncated font collection")?;
        let count = u32::from_be_bytes(count.try_into().unwrap());
        if face.index >= count {
            return Err("font collection index out of range".into());
        }
        let start = 12 + usize::try_from(face.index).map_err(|_| "font index overflow")? * 4;
        let offset: [u8; 4] = data
            .get(start..start + 4)
            .ok_or("truncated font collection directory")?
            .try_into()
            .unwrap();
        data.get_mut(12..16)
            .ok_or("truncated font collection directory")?
            .copy_from_slice(&offset);
    } else if face.index != 0 {
        return Err("non-collection font has a nonzero index".into());
    }
    FontFace::from_data_and_index(SharedBytes::from_vec(data.clone()), 0)
        .ok_or("invalid system font face")?;
    Ok(data)
}

fn load_system_face(name: &str) -> Result<FontDefinition, String> {
    let path = system_font_path(name)?;
    let data =
        SharedBytes::from_file_mmap_or_read(&path).map_err(|error| format!("{name}: {error}"))?;
    let count = if data.as_slice().starts_with(b"ttcf") {
        let count = data
            .as_slice()
            .get(8..12)
            .ok_or("truncated font collection")?;
        u32::from_be_bytes(count.try_into().unwrap())
    } else {
        1
    };
    for index in 0..count {
        let Some(face) = FontFace::from_data_and_index(data.clone(), index) else {
            continue;
        };
        let matches = face.with_ttf_parser_face(|face| {
            face.names().into_iter().any(|record| {
                // OpenType name ID 6 is the PostScript name, also returned by CoreText.
                record.name_id == 6 && record.to_string().as_deref() == Some(name)
            })
        });
        if matches {
            return Ok(FontDefinition {
                data,
                index,
                ascender_fudge_in_ems: 0.0,
                descender_fudge_in_ems: 0.0,
                weight: None,
                variations: Vec::new(),
            });
        }
    }
    Err(format!("{name}: matching collection face not found"))
}

// Small, private bindings to stable public APIs available on both Apple targets.
type CFRef = *const c_void;
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontCreateWithName(name: CFRef, size: f64, matrix: *const c_void) -> CFRef;
    fn CTFontCopyPostScriptName(font: CFRef) -> CFRef;
    fn CTFontCopyAttribute(font: CFRef, attribute: CFRef) -> CFRef;
    static kCTFontURLAttribute: CFRef;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(allocator: CFRef, text: *const c_char, encoding: u32) -> CFRef;
    fn CFEqual(a: CFRef, b: CFRef) -> u8;
    fn CFGetTypeID(value: CFRef) -> usize;
    fn CFURLGetTypeID() -> usize;
    fn CFURLGetFileSystemRepresentation(
        url: CFRef,
        resolve: u8,
        buffer: *mut u8,
        length: isize,
    ) -> u8;
    fn CFRelease(value: CFRef);
}

struct OwnedCf(CFRef);
impl OwnedCf {
    fn checked(value: CFRef) -> Result<Self, String> {
        if value.is_null() {
            Err("CoreText returned no font resource".into())
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: each instance owns one non-null Create/Copy-rule reference.
        unsafe { CFRelease(self.0) }
    }
}

fn system_font_path(name: &str) -> Result<PathBuf, String> {
    let name = CString::new(name).map_err(|error| error.to_string())?;
    // SAFETY: CoreFoundation objects remain alive throughout their use. All
    // pointers are checked, the attribute's type is validated, and CFURL receives
    // the actual writable buffer length. RAII releases every owned reference.
    unsafe {
        let requested = OwnedCf::checked(CFStringCreateWithCString(
            ptr::null(),
            name.as_ptr(),
            0x08000100,
        ))?;
        let font = OwnedCf::checked(CTFontCreateWithName(requested.0, 16.0, ptr::null()))?;
        let actual = OwnedCf::checked(CTFontCopyPostScriptName(font.0))?;
        if CFEqual(requested.0, actual.0) == 0 {
            return Err("CoreText substituted a different font".into());
        }
        let url = OwnedCf::checked(CTFontCopyAttribute(font.0, kCTFontURLAttribute))?;
        if CFGetTypeID(url.0) != CFURLGetTypeID() {
            return Err("font URL is not a CFURL".into());
        }
        let mut buffer = vec![0; libc::PATH_MAX as usize];
        if CFURLGetFileSystemRepresentation(url.0, 1, buffer.as_mut_ptr(), buffer.len() as isize)
            == 0
        {
            return Err("font URL has no readable filesystem path".into());
        }
        let length = buffer
            .iter()
            .position(|byte| *byte == 0)
            .ok_or("unterminated font path")?;
        buffer.truncate(length);
        Ok(PathBuf::from(OsString::from_vec(buffer)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_pingfang_faces_cover_english_and_chinese() {
        if !installed() {
            eprintln!("skipping: PingFang SC is not installed on this machine");
            return;
        }
        let mut indices = Vec::new();
        for name in [REGULAR, SEMIBOLD] {
            let definition = load_system_face(name).expect("installed PingFang face");
            indices.push(definition.index);
            let face = FontFace::from_data_and_index(definition.data, definition.index).unwrap();
            face.with_ttf_parser_face(|face| {
                for character in "Robrix Chats 0123 中文聊天简体繁體，。！？".chars() {
                    assert!(
                        face.glyph_index(character).is_some(),
                        "{name} missing {character}"
                    );
                    if !character.is_whitespace() {
                        assert!(
                            face.glyph_bounding_box(face.glyph_index(character).unwrap())
                                .is_some(),
                            "{name} has no drawable outline for {character}"
                        );
                    }
                }
            });
        }
        assert_ne!(
            indices[0], indices[1],
            "regular and semibold must select distinct TTC faces"
        );
    }

    #[test]
    fn missing_font_does_not_silently_use_coretext_substitution() {
        assert!(system_font_path("Robrix-DeliberatelyMissingFont").is_err());
    }

    #[test]
    fn theme_prefers_pingfang_and_preserves_fallback_after_widget_registration() {
        if !installed() {
            eprintln!("skipping: PingFang SC is not installed on this machine");
            return;
        }
        use makepad_draw::text::fonts::Fonts;
        use std::cell::RefCell;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.with_vm(|vm| {
            makepad_widgets::theme_mod(vm);
            script_eval!(vm, { mod.theme = mod.themes.light });
            install(vm);
            install(vm); // Repeated installation must not duplicate the primary.
            makepad_widgets::widgets_mod(vm);
            for (value, expected) in [
                (script_eval!(vm, { mod.theme.font_regular }), REGULAR),
                (script_eval!(vm, { mod.theme.font_bold }), SEMIBOLD),
                (
                    script_eval!(vm, { mod.widgets.Label.draw_text.text_style }),
                    REGULAR,
                ),
                (
                    script_eval!(vm, { mod.widgets.TextInput.draw_text.text_style }),
                    REGULAR,
                ),
            ] {
                let style = TextStyle::script_from_value(vm, value);
                assert_eq!(
                    style
                        .font_family
                        .member_ids()
                        .filter(|id| *id == "apple_system")
                        .count(),
                    1
                );
                // Several draws, including resource-generation changes, must
                // keep the same declared primary and usable fallback chain.
                for _ in 0..3 {
                    vm.cx().script_data.resources.bump_generation();
                    vm.with_cx_mut(|cx| style.ensure_fonts_loaded(cx));
                    let fonts = vm.cx_mut().get_global::<Rc<RefCell<Fonts>>>().clone();
                    let family = fonts
                        .borrow_mut()
                        .get_or_load_font_family(style.font_family_id());
                    assert!(family.fonts().len() > 1);
                    let shaped = family.get_or_shape("English 中文繁體 0123".into());
                    for glyph in &shaped.glyphs {
                        assert_ne!(glyph.id, 0);
                        let face =
                            FontFace::from_data_and_index(glyph.font.data().clone(), 0).unwrap();
                        face.with_ttf_parser_face(|f| {
                            assert!(
                                f.names().into_iter().any(|n| n.name_id == 6
                                    && n.to_string().as_deref() == Some(expected))
                            )
                        });
                    }
                }
            }
        });
    }

    #[test]
    fn collection_view_preserves_selected_face_glyphs_and_tables() {
        if !installed() {
            eprintln!("skipping: PingFang SC is not installed on this machine");
            return;
        }
        for name in [REGULAR, SEMIBOLD] {
            let source = load_system_face(name).unwrap();
            let original =
                FontFace::from_data_and_index(source.data.clone(), source.index).unwrap();
            let bytes = face_at_index_zero(&source).unwrap();
            let view = FontFace::from_data_and_index(SharedBytes::from_vec(bytes), 0).unwrap();
            original.with_ttf_parser_face(|a| {
                view.with_ttf_parser_face(|b| {
                    assert_eq!(a.units_per_em(), b.units_per_em());
                    for ch in "English 中文繁體 0123".chars() {
                        let glyph = a.glyph_index(ch).unwrap();
                        assert_eq!(Some(glyph), b.glyph_index(ch));
                        assert_eq!(a.glyph_hor_advance(glyph), b.glyph_hor_advance(glyph));
                        assert_eq!(a.glyph_bounding_box(glyph), b.glyph_bounding_box(glyph));
                    }
                })
            });
        }
    }

    #[test]
    fn collection_view_rejects_invalid_indices_and_truncated_directories() {
        // The directory checks run before any face is parsed, so a bare
        // two-entry collection header exercises them on every machine.
        let mut data = b"ttcf\x00\x01\x00\x00\x00\x00\x00\x02".to_vec();
        data.extend_from_slice(&20u32.to_be_bytes());
        data.extend_from_slice(&40u32.to_be_bytes());
        let mut face = FontDefinition {
            data: SharedBytes::from_vec(data),
            index: u32::MAX,
            ascender_fudge_in_ems: 0.0,
            descender_fudge_in_ems: 0.0,
            weight: None,
            variations: Vec::new(),
        };
        assert!(face_at_index_zero(&face).is_err());
        face.index = 0;
        face.data = SharedBytes::from_vec(b"ttcf".to_vec());
        assert!(face_at_index_zero(&face).is_err());
    }
}
