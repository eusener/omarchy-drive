//! Atalhos locais para documentos nativos do Google (Docs, Planilhas...).
//!
//! Esses documentos não têm conteúdo binário para baixar. Cada um vira um
//! arquivo `Nome.gdoc` (ou `.gsheet`, `.gslides`...) com um JSON pequeno e
//! determinístico: o mesmo id e tipo geram sempre os mesmos bytes, então o
//! hash do atalho serve para detectar edições locais como em qualquer arquivo.

/// (sufixo do mimeType, extensão, prefixo da URL de edição).
const TYPES: &[(&str, &str, &str)] = &[
    ("document", "gdoc", "https://docs.google.com/document/d/"),
    (
        "spreadsheet",
        "gsheet",
        "https://docs.google.com/spreadsheets/d/",
    ),
    (
        "presentation",
        "gslides",
        "https://docs.google.com/presentation/d/",
    ),
    ("drawing", "gdraw", "https://docs.google.com/drawings/d/"),
    ("form", "gform", "https://docs.google.com/forms/d/"),
    ("map", "gmap", "https://www.google.com/maps/d/edit?mid="),
];
/// Demais tipos nativos (Sites, Jamboard, atalhos do Drive...).
const OTHER_EXT: &str = "glink";
const NATIVE_PREFIX: &str = "application/vnd.google-apps.";

/// Hosts que `open-url` aceita abrir a partir de um atalho.
pub const ALLOWED_URL_PREFIXES: &[&str] = &[
    "https://docs.google.com/",
    "https://drive.google.com/",
    "https://www.google.com/maps/d/",
];

/// Extensão do atalho para um mimeType nativo do Google.
pub fn ext_for_mime(mime: &str) -> &'static str {
    let kind = mime.strip_prefix(NATIVE_PREFIX).unwrap_or(mime);
    TYPES
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map(|(_, e, _)| *e)
        .unwrap_or(OTHER_EXT)
}

fn ext_of(path: &str) -> Option<&str> {
    let name = crate::model::name_of(path);
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()).then_some(ext)
}

/// O caminho tem extensão de atalho?
pub fn is_shortcut_path(path: &str) -> bool {
    ext_of(path).is_some_and(|e| e == OTHER_EXT || TYPES.iter().any(|(_, x, _)| *x == e))
}

/// Nome do documento no Drive a partir do nome do atalho (`Relatório.gdoc` → `Relatório`).
pub fn strip_ext(name: &str) -> &str {
    if is_shortcut_path(name) {
        name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name)
    } else {
        name
    }
}

/// URL de edição do documento.
pub fn url_for(id: &str, ext: &str) -> String {
    match TYPES.iter().find(|(_, e, _)| *e == ext) {
        Some((_, _, prefix)) if prefix.ends_with('=') => format!("{prefix}{id}"),
        Some((_, _, prefix)) => format!("{prefix}{id}/edit"),
        None => format!("https://drive.google.com/open?id={id}"),
    }
}

/// Bytes do atalho de um documento (determinístico).
pub fn content(id: &str, name: &str) -> Vec<u8> {
    let ext = ext_of(name).unwrap_or(OTHER_EXT);
    let v = serde_json::json!({ "doc_id": id, "type": ext, "url": url_for(id, ext) });
    // serde_json ordena as chaves (BTreeMap): saída estável.
    let mut s = serde_json::to_string_pretty(&v).unwrap_or_default();
    s.push('\n');
    s.into_bytes()
}

/// URL contida num atalho, se for de um host permitido.
pub fn url_from(bytes: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let url = v.get("url")?.as_str()?;
    ALLOWED_URL_PREFIXES
        .iter()
        .any(|p| url.starts_with(p))
        .then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensoes_por_tipo() {
        assert_eq!(ext_for_mime("application/vnd.google-apps.document"), "gdoc");
        assert_eq!(
            ext_for_mime("application/vnd.google-apps.spreadsheet"),
            "gsheet"
        );
        assert_eq!(
            ext_for_mime("application/vnd.google-apps.presentation"),
            "gslides"
        );
        assert_eq!(ext_for_mime("application/vnd.google-apps.jam"), "glink");
    }

    #[test]
    fn caminhos_de_atalho() {
        assert!(is_shortcut_path("A/Relatório.gdoc"));
        assert!(is_shortcut_path("x.glink"));
        assert!(!is_shortcut_path("A/nota.txt"));
        assert!(!is_shortcut_path(".gdoc"));
        assert_eq!(strip_ext("Relatório final.gdoc"), "Relatório final");
        assert_eq!(strip_ext("nota.txt"), "nota.txt");
    }

    #[test]
    fn conteudo_deterministico_e_url() {
        let a = content("ID1", "Relatório.gdoc");
        assert_eq!(a, content("ID1", "Outro nome.gdoc"));
        assert_ne!(a, content("ID2", "Relatório.gdoc"));
        assert_eq!(
            url_from(&a).unwrap(),
            "https://docs.google.com/document/d/ID1/edit"
        );
        assert_eq!(
            url_from(&content("X", "a.glink")).unwrap(),
            "https://drive.google.com/open?id=X"
        );
        assert!(url_from(br#"{"url":"https://evil.example/"}"#).is_none());
        assert!(url_from(b"lixo").is_none());
    }
}
