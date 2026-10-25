use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_PRODUCTION_WEB");
    if env::var_os("CARGO_FEATURE_PRODUCTION_WEB").is_none() {
        return;
    }
    let dist = Path::new("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    for directory in [dist.to_path_buf(), dist.join("assets")] {
        assert!(
            fs::symlink_metadata(directory)
                .expect("production-web requires a freshly built web/dist")
                .file_type()
                .is_dir(),
            "built asset directories must not be symlinks"
        );
    }
    let mut entries = vec![("index.html".to_owned(), "text/html; charset=utf-8")];
    let mut assets: Vec<_> = fs::read_dir(dist.join("assets"))
        .expect("production-web requires npm ci --ignore-scripts && npm run build in web/")
        .map(|entry| entry.expect("read built asset"))
        .collect();
    assets.sort_by_key(|entry| entry.file_name());
    for entry in assets {
        let name = entry.file_name().into_string().expect("ASCII asset name");
        let mime = asset_mime(&name);
        entries.push((format!("assets/{name}"), mime));
    }
    assert!(entries.len() >= 3, "expected built HTML, JS and CSS");
    let mut generated = String::from("pub const FILES: &[(&str, &str, &[u8])] = &[\n");
    for (name, mime) in entries {
        let path = dist.join(&name);
        assert!(
            fs::symlink_metadata(&path)
                .expect("built file")
                .file_type()
                .is_file(),
            "assets must be regular files"
        );
        assert!(
            fs::metadata(&path).unwrap().len() <= 1_048_576,
            "asset exceeds default response budget"
        );
        let path = fs::canonicalize(path).unwrap();
        generated.push_str(&format!(
            "({name:?}, {mime:?}, include_bytes!({:?})),\n",
            path.to_str().unwrap()
        ));
    }
    generated.push_str("];\n");
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("web_assets.rs"),
        generated,
    )
    .unwrap();
}

fn asset_mime(name: &str) -> &'static str {
    let (stem, extension) = name.rsplit_once('.').expect("asset extension");
    let (_, hash) = stem.split_once('-').expect("Vite content hash");
    assert!(
        hash.len() >= 8
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "unexpected asset name"
    );
    let mime = match extension {
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        _ => panic!("only production JS/CSS assets may be embedded"),
    };
    mime
}

#[cfg(test)]
mod tests {
    use super::asset_mime;

    #[test]
    fn vite_hash_alphabet_includes_internal_hyphens_and_underscores() {
        for name in [
            "index-abcdefgh.js",
            "index-abcd-fgh.js",
            "index-abcd_fgh.js",
        ] {
            assert_eq!(asset_mime(name), "text/javascript; charset=utf-8");
        }
        assert_eq!(asset_mime("index-abcd-fgh.css"), "text/css; charset=utf-8");
    }

    #[test]
    #[should_panic(expected = "unexpected asset name")]
    fn encoded_path_is_not_a_public_asset() {
        asset_mime("index-abcd%2ffgh.js");
    }
}
