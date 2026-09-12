/// HTML email wrapper using the same IIEC logo as the parent portal.
pub fn branded_html(body: &str, frontend: &str) -> String {
    with_link(
        body,
        frontend,
        "Buka portal parent",
        &format!("{}/parent/dashboard", frontend.trim_end_matches('/')),
    )
}

pub fn with_link(body: &str, frontend: &str, label: &str, link: &str) -> String {
    fn escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    }
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"></head><body><div style=\"font-family:Arial,sans-serif;max-width:640px;margin:24px auto;padding:24px;background:#fff;color:#16273b\"><img src=\"{}/brand/iiec-logo.png\" alt=\"IIEC — International Islamic Education Council\" width=\"112\" style=\"display:block;margin:0 auto 24px;height:auto\"><pre style=\"white-space:pre-wrap;font-family:Arial,sans-serif;font-size:15px;line-height:1.7\">{}</pre><p><a href=\"{}\" style=\"display:inline-block;background:#16273b;color:#fff;padding:12px 20px;border-radius:6px;text-decoration:none\">{}</a></p><p style=\"border-top:1px solid #ddd;padding-top:16px;color:#687386\">IIEC School · Admissions</p></div></body></html>",escape(frontend.trim_end_matches('/')),escape(body),escape(link),escape(label))
}
