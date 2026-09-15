use chrono::{DateTime, Datelike, Utc};
use serde::Deserialize;

use crate::repositories::payment_repository::PaymentDocumentContext;

const PAGE_HEIGHT: f32 = 842.0;
const CONTACT_PHONE: &str = "+62 811 8888 3533";
const CONTACT_EMAIL: &str = "twsi-indonesia-jaya@theworldscholars.com";
const CONTACT_ADDRESS_1: &str = "Arkadia Green Park, Tower B-C, 5th Floor";
const CONTACT_ADDRESS_2: &str = "Jl. Let. Jend. TB Simatupang Kav. 88, Jakarta 12520";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaymentDocumentKind {
    Invoice,
    Receipt,
}

pub fn is_document_available(kind: PaymentDocumentKind, payment_status: &str) -> bool {
    kind == PaymentDocumentKind::Invoice
        || (kind == PaymentDocumentKind::Receipt && payment_status == "paid")
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LineItem {
    label: String,
    amount: i64,
}

#[derive(Clone, Copy)]
struct Color(f32, f32, f32);

struct Canvas {
    commands: String,
}

impl Canvas {
    fn new() -> Self {
        Self {
            commands: String::new(),
        }
    }

    fn rect(&mut self, x: f32, top: f32, width: f32, height: f32, color: Color) {
        let y = PAGE_HEIGHT - top - height;
        self.commands.push_str(&format!(
            "{:.3} {:.3} {:.3} rg {:.2} {:.2} {:.2} {:.2} re f\n",
            color.0, color.1, color.2, x, y, width, height
        ));
    }

    fn line(&mut self, x1: f32, top1: f32, x2: f32, top2: f32, width: f32, color: Color) {
        self.commands.push_str(&format!(
            "{:.3} {:.3} {:.3} RG {:.2} w {:.2} {:.2} m {:.2} {:.2} l S\n",
            color.0,
            color.1,
            color.2,
            width,
            x1,
            PAGE_HEIGHT - top1,
            x2,
            PAGE_HEIGHT - top2
        ));
    }

    fn circle(&mut self, cx: f32, top: f32, radius: f32, color: Color) {
        let cy = PAGE_HEIGHT - top;
        let k = radius * 0.552_284_8;
        self.commands.push_str(&format!(
            "{:.3} {:.3} {:.3} rg {:.2} {:.2} m {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c f\n",
            color.0, color.1, color.2,
            cx + radius, cy,
            cx + radius, cy + k, cx + k, cy + radius, cx, cy + radius,
            cx - k, cy + radius, cx - radius, cy + k, cx - radius, cy,
            cx - radius, cy - k, cx - k, cy - radius, cx, cy - radius,
            cx + k, cy - radius, cx + radius, cy - k, cx + radius, cy
        ));
    }

    fn text(&mut self, x: f32, top: f32, size: f32, bold: bool, color: Color, value: &str) {
        let font = if bold { "F2" } else { "F1" };
        self.commands.push_str(&format!(
            "BT /{} {:.2} Tf {:.3} {:.3} {:.3} rg 1 0 0 1 {:.2} {:.2} Tm ({}) Tj ET\n",
            font,
            size,
            color.0,
            color.1,
            color.2,
            x,
            PAGE_HEIGHT - top,
            escape_pdf_text(value)
        ));
    }

    fn text_right(
        &mut self,
        right: f32,
        top: f32,
        size: f32,
        bold: bool,
        color: Color,
        value: &str,
    ) {
        self.text(
            right - estimated_text_width(value, size, bold),
            top,
            size,
            bold,
            color,
            value,
        );
    }
}

pub fn render_payment_document(
    context: &PaymentDocumentContext,
    kind: PaymentDocumentKind,
) -> Vec<u8> {
    let hydrated = hydrate_offer_items(context);
    let context = &hydrated;
    let details = vec![];
    let school = school_brand(&context.school_code);
    let mut canvas = Canvas::new();
    if let Some(background) = template_background(&context.school_code, kind) {
        match kind {
            PaymentDocumentKind::Invoice => draw_template_invoice(&mut canvas, context),
            PaymentDocumentKind::Receipt => draw_template_receipt(&mut canvas, context),
        }
        build_pdf(canvas.commands.as_bytes(), Some(background), &details)
    } else {
        // IIBS has no approved Canva sample yet, so retain the neutral generated
        // layout instead of displaying another school's logo and stationery.
        draw_brand_header(&mut canvas, school);
        match kind {
            PaymentDocumentKind::Invoice => draw_invoice(&mut canvas, context, school),
            PaymentDocumentKind::Receipt => draw_receipt(&mut canvas, context, school),
        }
        build_pdf(canvas.commands.as_bytes(), None, &details)
    }
}

#[derive(Clone, Copy)]
struct TemplateBackground {
    jpeg: &'static [u8],
    width: u32,
    height: u32,
}

fn template_background(school_code: &str, kind: PaymentDocumentKind) -> Option<TemplateBackground> {
    let normalized = school_code
        .trim()
        .trim_start_matches("SCH-")
        .trim_start_matches("SCHOOL-")
        .to_uppercase();
    let jpeg = match (normalized.as_str(), kind) {
        ("IISS", PaymentDocumentKind::Invoice) => {
            include_bytes!("../../assets/payment-documents/iiss-invoice.jpg").as_slice()
        }
        ("IISS", PaymentDocumentKind::Receipt) => {
            include_bytes!("../../assets/payment-documents/iiss-receipt.jpg").as_slice()
        }
        ("IIHS", PaymentDocumentKind::Invoice) => {
            include_bytes!("../../assets/payment-documents/iihs-invoice.jpg").as_slice()
        }
        ("IIHS", PaymentDocumentKind::Receipt) => {
            include_bytes!("../../assets/payment-documents/iihs-receipt.jpg").as_slice()
        }
        _ => return None,
    };
    Some(TemplateBackground {
        jpeg,
        width: 2481,
        height: 3509,
    })
}

fn draw_template_invoice(canvas: &mut Canvas, context: &PaymentDocumentContext) {
    let ink = Color(0.10, 0.10, 0.10);
    let white = Color(1.0, 1.0, 1.0);
    // Trim the raster title and replacement number band to one straight edge.
    // Their original left boundaries differ by about 0.7pt between samples.
    canvas.rect(356.0, 170.0, 3.0, 85.5, white);
    draw_template_recipient(canvas, context, "Invoice to :", ink);
    canvas.text(
        358.0,
        155.0,
        9.5,
        false,
        ink,
        &format!("Date : {}", format_date(context.created_at.as_deref())),
    );
    canvas.text(
        372.0,
        239.0,
        8.5,
        false,
        white,
        &format!("Invoice No : {}", fit(&context.payment.payment_id, 35)),
    );

    draw_template_line_items(canvas, context, true, ink);
    draw_centered_total(
        canvas,
        379.0,
        157.0,
        12.0,
        white,
        &format!(
            "Total : {}",
            format_currency(context.payment.amount, &context.payment.currency)
        ),
    );

    canvas.text(52.0, 565.0, 11.5, true, ink, "Payment Method");
    if let Some(bank) = non_empty(context.payment.bank_name.as_deref()) {
        canvas.text(
            52.0,
            585.0,
            9.0,
            false,
            ink,
            &format!("Bank Name : {}", fit(bank, 34)),
        );
        if let Some(account) = non_empty(context.payment.bank_account_number.as_deref()) {
            canvas.text(
                52.0,
                601.0,
                9.0,
                false,
                ink,
                &format!("Account No : {}", fit(account, 31)),
            );
        }
    } else {
        canvas.text(
            52.0,
            585.0,
            9.0,
            false,
            ink,
            &format!("Payment Method : {}", payment_method_label(context)),
        );
    }
}

fn draw_template_receipt(canvas: &mut Canvas, context: &PaymentDocumentContext) {
    let ink = Color(0.10, 0.10, 0.10);
    let white = Color(1.0, 1.0, 1.0);
    draw_template_recipient(canvas, context, "Received from :", ink);
    canvas.text(
        322.0,
        155.0,
        9.5,
        false,
        ink,
        &format!("Date : {}", format_date(context.payment.paid_at.as_deref())),
    );
    let receipt_number =
        non_empty(context.payment.receipt_ref.as_deref()).unwrap_or(&context.payment.payment_id);
    canvas.text(
        336.0,
        239.0,
        8.5,
        false,
        white,
        &format!("Receipt No : {}", fit(receipt_number, 34)),
    );

    draw_template_line_items(canvas, context, false, ink);
    let received = received_amount(context);
    draw_centered_total(
        canvas,
        219.0,
        317.0,
        11.0,
        white,
        &format!(
            "TOTAL PAYMENT RECEIVED: {}",
            format_currency(received, &context.payment.currency)
        ),
    );

    canvas.text(52.0, 562.0, 11.5, true, ink, "Payment Information");
    canvas.text(
        52.0,
        583.0,
        9.0,
        false,
        ink,
        &format!(
            "Payment Date: {}",
            format_date(context.payment.paid_at.as_deref())
        ),
    );
    canvas.text(
        52.0,
        599.0,
        9.0,
        false,
        ink,
        &format!("Payment Method: {}", payment_method_label(context)),
    );
    if let Some(bank) = non_empty(context.payment.bank_name.as_deref()) {
        canvas.text(
            52.0,
            615.0,
            9.0,
            false,
            ink,
            &format!("Bank Name : {}", fit(bank, 31)),
        );
    }
    if let Some(account) = non_empty(context.payment.bank_account_number.as_deref()) {
        canvas.text(
            52.0,
            631.0,
            9.0,
            false,
            ink,
            &format!("Account No : {}", fit(account, 30)),
        );
    }
    canvas.text(288.0, 562.0, 11.5, true, ink, "Remarks");
    canvas.text(
        288.0,
        583.0,
        8.5,
        false,
        ink,
        "Payment is received in full for the above-mentioned invoice.",
    );
    canvas.text(288.0, 607.0, 11.0, true, ink, "Related Invoice No.");
    canvas.text(
        288.0,
        624.0,
        8.5,
        false,
        ink,
        &fit(&context.payment.payment_id, 39),
    );
}

fn draw_template_recipient(
    canvas: &mut Canvas,
    context: &PaymentDocumentContext,
    heading: &str,
    ink: Color,
) {
    canvas.text(49.0, 155.0, 9.5, true, ink, heading);
    canvas.text(49.0, 178.0, 15.0, true, ink, &fit(&context.parent_name, 38));
    canvas.text(49.0, 198.0, 9.5, true, ink, "Parent/ Guardian of:");
    let students = student_label(context);
    for (index, line) in wrap(&students, 42, 2).iter().enumerate() {
        canvas.text(49.0, 218.0 + index as f32 * 14.0, 10.0, true, ink, line);
    }
    if let Some(email) = non_empty(Some(context.parent_email.as_str())) {
        canvas.text(49.0, 236.0, 8.5, false, ink, &fit(email, 46));
    }
    if let Some(location) = non_empty(context.parent_location.as_deref()) {
        canvas.text(49.0, 251.0, 8.5, false, ink, &fit(location, 51));
    }
}

fn draw_template_line_items(
    canvas: &mut Canvas,
    context: &PaymentDocumentContext,
    show_price: bool,
    ink: Color,
) {
    let items = invoice_line_items(context);
    let displayed = summary_items(&items, 4);
    for (index, item) in displayed.iter().enumerate() {
        let baseline = 330.0 + index as f32 * 32.0;
        canvas.text(75.0, baseline, 9.0, true, ink, &fit(&item.label, 43));
        if show_price {
            canvas.text(321.0, baseline, 9.0, true, ink, "1");
            canvas.text_right(437.0, baseline, 9.0, true, ink, &format_money(item.amount));
        }
        canvas.text_right(521.0, baseline, 9.0, true, ink, &format_money(item.amount));
    }

    let subtotal = context
        .payment
        .gross_amount
        .unwrap_or(context.payment.amount);
    let discount = context.payment.discount_amount.unwrap_or(0);
    canvas.line(61.0, 432.0, 536.0, 432.0, 1.1, ink);
    canvas.text(75.0, 447.0, 9.0, true, ink, "Subtotal");
    canvas.text_right(521.0, 447.0, 9.0, true, ink, &format_money(subtotal));
    if discount > 0 {
        canvas.text(75.0, 477.0, 9.0, true, ink, "Discount");
        canvas.text_right(
            // Accounting parenthesis hangs outside the aligned last digit.
            521.0 + estimated_text_width(")", 9.0, true),
            477.0,
            9.0,
            true,
            ink,
            &format!("({})", format_money(discount)),
        );
    }
    canvas.line(61.0, 486.0, 536.0, 486.0, 1.1, ink);
}

fn received_amount(context: &PaymentDocumentContext) -> i64 {
    context
        .payment
        .amount_verified
        .or(context.payment.amount_submitted)
        .filter(|value| *value > 0)
        .unwrap_or(context.payment.amount)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

// Standard Helvetica-Bold AFM advance widths, ASCII 32..126 (1/1000 em).
const HELVETICA_BOLD_WIDTHS: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

fn total_text_width(value: &str, size: f32) -> f32 {
    value
        .bytes()
        .map(|byte| {
            HELVETICA_BOLD_WIDTHS
                .get(byte.saturating_sub(32) as usize)
                .copied()
                .unwrap_or(556) as f32
        })
        .sum::<f32>()
        * size
        / 1000.0
}

fn draw_centered_total(
    canvas: &mut Canvas,
    left: f32,
    width: f32,
    size: f32,
    color: Color,
    text: &str,
) {
    // Fit large amounts inside the unchanged Canva box with 10pt side padding.
    let size = size.min((width - 20.0) / total_text_width(text, 1.0));
    let x = left + (width - total_text_width(text, size)) / 2.0;
    // Box y=505..540; AFM cap height=718 and visible 'p' descender=-207.
    let baseline = 522.5 + size * (718.0 - 207.0) / 2000.0;
    canvas.text(x, baseline, size, true, color, text);
}

fn estimated_text_width(value: &str, size: f32, bold: bool) -> f32 {
    const REGULAR: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ];
    let widths = if bold {
        &HELVETICA_BOLD_WIDTHS
    } else {
        &REGULAR
    };
    value
        .chars()
        .map(|ch| {
            let ch = if ch.is_ascii() && !ch.is_control() {
                ch
            } else {
                '?'
            };
            widths[(ch as usize) - 32] as f32
        })
        .sum::<f32>()
        * size
        / 1000.0
}

fn draw_brand_header(canvas: &mut Canvas, school: SchoolBrand) {
    let ink = Color(0.08, 0.09, 0.10);
    canvas.circle(66.0, 64.0, 31.0, school.accent);
    canvas.text(48.0, 69.0, 13.0, true, Color(1.0, 1.0, 1.0), school.code);
    canvas.text(110.0, 56.0, 27.0, true, ink, school.heading);
    canvas.text(111.0, 76.0, 10.5, true, ink, school.name);
    canvas.circle(532.0, 62.0, 27.0, Color(0.78, 0.76, 0.72));
    canvas.text(517.0, 66.0, 11.0, true, ink, "IIEC");
}

fn draw_invoice(canvas: &mut Canvas, context: &PaymentDocumentContext, school: SchoolBrand) {
    let ink = Color(0.12, 0.12, 0.13);
    let muted = Color(0.30, 0.31, 0.32);
    canvas.text(58.0, 132.0, 9.5, true, ink, "Invoice to:");
    canvas.text(58.0, 154.0, 16.0, true, ink, &fit(&context.parent_name, 38));
    canvas.text(58.0, 173.0, 9.5, true, ink, "Parent / Guardian of:");
    let students = student_label(context);
    for (index, line) in wrap(&students, 43, 2).iter().enumerate() {
        canvas.text(58.0, 192.0 + index as f32 * 14.0, 10.0, true, ink, line);
    }
    if !context.parent_email.trim().is_empty() {
        canvas.text(
            58.0,
            222.0,
            8.5,
            false,
            muted,
            &fit(&context.parent_email, 50),
        );
    }
    if let Some(location) = context
        .parent_location
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        canvas.text(58.0, 236.0, 8.5, false, muted, &fit(location, 50));
    }

    canvas.text(
        360.0,
        132.0,
        9.5,
        false,
        ink,
        &format!("Date: {}", format_date(context.created_at.as_deref())),
    );
    canvas.rect(360.0, 146.0, 235.0, 78.0, school.accent);
    canvas.text(378.0, 184.0, 32.0, false, Color(1.0, 1.0, 1.0), "INVOICE");
    canvas.text(
        378.0,
        207.0,
        8.6,
        false,
        Color(1.0, 1.0, 1.0),
        &format!("Invoice No: {}", fit(&context.payment.payment_id, 34)),
    );

    let items = invoice_line_items(context);
    let table_top = 258.0;
    canvas.rect(0.0, table_top, 595.0, 32.0, school.accent);
    canvas.text(
        62.0,
        table_top + 21.0,
        10.0,
        true,
        Color(1.0, 1.0, 1.0),
        "DESCRIPTION",
    );
    canvas.text(
        382.0,
        table_top + 21.0,
        10.0,
        true,
        Color(1.0, 1.0, 1.0),
        "QTY",
    );
    canvas.text(
        454.0,
        table_top + 21.0,
        10.0,
        true,
        Color(1.0, 1.0, 1.0),
        "TOTAL",
    );
    let mut row_top = table_top + 32.0;
    let displayed = summary_items(&items, 7);
    for (index, item) in displayed.iter().enumerate() {
        if index % 2 == 1 {
            canvas.rect(0.0, row_top, 595.0, 30.0, Color(0.96, 0.96, 0.95));
        }
        canvas.text(62.0, row_top + 20.0, 9.0, true, ink, &fit(&item.label, 50));
        canvas.text(388.0, row_top + 20.0, 9.0, true, ink, "1");
        canvas.text(
            472.0,
            row_top + 20.0,
            9.0,
            true,
            ink,
            &format_money(item.amount),
        );
        row_top += 30.0;
    }
    let subtotal = context
        .payment
        .gross_amount
        .unwrap_or(context.payment.amount);
    let discount = context.payment.discount_amount.unwrap_or(0);
    canvas.line(60.0, row_top + 3.0, 535.0, row_top + 3.0, 1.2, ink);
    canvas.text(62.0, row_top + 20.0, 9.0, true, ink, "Subtotal");
    canvas.text(
        472.0,
        row_top + 20.0,
        9.0,
        true,
        ink,
        &format_money(subtotal),
    );
    row_top += 28.0;
    if discount > 0 {
        canvas.rect(0.0, row_top, 595.0, 28.0, Color(0.96, 0.96, 0.95));
        canvas.text(62.0, row_top + 19.0, 9.0, true, ink, "Discount");
        canvas.text(
            472.0,
            row_top + 19.0,
            9.0,
            true,
            ink,
            &format!("({})", format_money(discount)),
        );
        row_top += 28.0;
    }
    canvas.line(60.0, row_top + 3.0, 535.0, row_top + 3.0, 1.2, ink);
    canvas.rect(375.0, row_top + 22.0, 160.0, 36.0, school.accent);
    canvas.text(
        390.0,
        row_top + 46.0,
        12.0,
        true,
        Color(1.0, 1.0, 1.0),
        &format!(
            "TOTAL: {}",
            format_currency(context.payment.amount, &context.payment.currency)
        ),
    );

    let footer_top = (row_top + 90.0).min(655.0);
    draw_payment_method(canvas, context, footer_top, ink, muted);
    draw_contact(canvas, 58.0, footer_top + 76.0, ink, muted);
    canvas.rect(0.0, 818.0, 595.0, 24.0, school.accent);
}

fn draw_receipt(canvas: &mut Canvas, context: &PaymentDocumentContext, school: SchoolBrand) {
    let ink = Color(0.12, 0.12, 0.13);
    let muted = Color(0.30, 0.31, 0.32);
    canvas.text(48.0, 124.0, 9.5, true, ink, "Received from:");
    canvas.text(48.0, 147.0, 15.0, true, ink, &fit(&context.parent_name, 38));
    canvas.text(48.0, 166.0, 9.5, true, ink, "Parent / Guardian of:");
    let students = student_label(context);
    for (index, line) in wrap(&students, 40, 2).iter().enumerate() {
        canvas.text(48.0, 185.0 + index as f32 * 14.0, 10.0, true, ink, line);
    }

    canvas.text(
        322.0,
        124.0,
        9.5,
        false,
        ink,
        &format!("Date: {}", format_date(context.payment.paid_at.as_deref())),
    );
    canvas.rect(322.0, 138.0, 273.0, 78.0, school.accent);
    canvas.text(
        340.0,
        177.0,
        24.0,
        false,
        Color(1.0, 1.0, 1.0),
        "PAYMENT RECEIPT",
    );
    let receipt_number = context
        .payment
        .receipt_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&context.payment.payment_id);
    canvas.text(
        340.0,
        200.0,
        8.6,
        false,
        Color(1.0, 1.0, 1.0),
        &format!("Receipt No: {}", fit(receipt_number, 35)),
    );

    canvas.rect(0.0, 247.0, 595.0, 30.0, school.accent);
    canvas.text(445.0, 267.0, 10.0, true, Color(1.0, 1.0, 1.0), "AMOUNT");
    canvas.rect(0.0, 277.0, 595.0, 34.0, Color(0.96, 0.96, 0.95));
    let received = context
        .payment
        .amount_verified
        .or(context.payment.amount_submitted)
        .filter(|value| *value > 0)
        .unwrap_or(context.payment.amount);
    canvas.text(466.0, 299.0, 9.5, true, ink, &format_money(received));
    canvas.line(60.0, 312.0, 535.0, 312.0, 1.2, ink);
    canvas.rect(293.0, 333.0, 242.0, 38.0, school.accent);
    canvas.text(
        307.0,
        358.0,
        12.0,
        true,
        Color(1.0, 1.0, 1.0),
        &format!(
            "TOTAL RECEIVED: {}",
            format_currency(received, &context.payment.currency)
        ),
    );

    canvas.text(52.0, 407.0, 12.0, true, ink, "Payment Information");
    canvas.text(
        52.0,
        428.0,
        9.0,
        false,
        ink,
        &format!(
            "Payment Date: {}",
            format_date(context.payment.paid_at.as_deref())
        ),
    );
    canvas.text(
        52.0,
        444.0,
        9.0,
        false,
        ink,
        &format!("Payment Method: {}", payment_method_label(context)),
    );
    if let Some(bank) = context
        .payment
        .bank_name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        canvas.text(
            52.0,
            460.0,
            9.0,
            false,
            ink,
            &format!("Bank: {}", fit(bank, 42)),
        );
    }
    if let Some(account) = context
        .payment
        .bank_account_number
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        canvas.text(
            52.0,
            476.0,
            9.0,
            false,
            ink,
            &format!("Account No: {}", fit(account, 36)),
        );
    }
    canvas.text(288.0, 407.0, 12.0, true, ink, "Remarks");
    canvas.text(
        288.0,
        428.0,
        9.0,
        false,
        ink,
        "Payment received for the invoice below.",
    );
    canvas.text(288.0, 454.0, 11.0, true, ink, "Related Invoice No.");
    canvas.text(288.0, 472.0, 9.0, false, ink, &context.payment.payment_id);

    canvas.rect(238.0, 520.0, 122.0, 54.0, Color(0.86, 0.10, 0.34));
    canvas.rect(243.0, 525.0, 112.0, 44.0, Color(1.0, 1.0, 1.0));
    canvas.text(
        254.0,
        552.0,
        14.0,
        true,
        Color(0.86, 0.10, 0.34),
        "PAID IN FULL",
    );
    canvas.text(420.0, 552.0, 8.0, true, school.accent, "DIGITALLY ISSUED");
    canvas.text(454.0, 570.0, 9.0, false, muted, "Finance");

    draw_contact(canvas, 52.0, 636.0, ink, muted);
    canvas.text(
        350.0,
        656.0,
        8.5,
        false,
        muted,
        "This receipt was generated from the verified payment record.",
    );
    canvas.text(
        350.0,
        672.0,
        8.5,
        false,
        muted,
        &format!("Payment ID: {}", context.payment.payment_id),
    );
    canvas.rect(0.0, 818.0, 595.0, 24.0, school.accent);
}

fn draw_payment_method(
    canvas: &mut Canvas,
    context: &PaymentDocumentContext,
    top: f32,
    ink: Color,
    muted: Color,
) {
    canvas.text(58.0, top, 12.0, true, ink, "Payment Method");
    canvas.text(
        58.0,
        top + 20.0,
        9.0,
        false,
        muted,
        &payment_method_label(context),
    );
    if let Some(bank) = context
        .payment
        .bank_name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        canvas.text(
            58.0,
            top + 37.0,
            9.0,
            false,
            muted,
            &format!("Bank: {}", fit(bank, 48)),
        );
    }
    if let Some(account) = context
        .payment
        .bank_account_number
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        canvas.text(
            58.0,
            top + 54.0,
            9.0,
            false,
            muted,
            &format!("Account No: {}", fit(account, 42)),
        );
    }
}

fn draw_contact(canvas: &mut Canvas, x: f32, top: f32, ink: Color, muted: Color) {
    canvas.text(x, top, 12.0, true, ink, "Contact Us");
    canvas.text(x, top + 20.0, 9.0, false, muted, CONTACT_PHONE);
    canvas.text(x, top + 36.0, 9.0, false, muted, CONTACT_EMAIL);
    canvas.text(x, top + 52.0, 9.0, false, muted, CONTACT_ADDRESS_1);
    canvas.text(x, top + 68.0, 9.0, false, muted, CONTACT_ADDRESS_2);
}

/// Only render the full commitment as payable when this payment covers it in full.
/// Partial payments retain their due-now line instead of falsely receipting all fees.
fn hydrate_offer_items(context: &PaymentDocumentContext) -> PaymentDocumentContext {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Pricing {
        currency: String,
        line_items: Vec<LineItem>,
        gross_total: i64,
        discount_total: i64,
        net_commitment: i64,
        amount_due_now: i64,
    }
    let mut result = context.clone();
    let Some(p) = context
        .pricing_snapshot_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Pricing>(json).ok())
    else {
        return result;
    };
    if p.currency != context.payment.currency
        || p.amount_due_now != context.payment.amount
        || p.net_commitment != p.amount_due_now
        || p.discount_total < 0
        || p.gross_total.checked_sub(p.discount_total) != Some(p.net_commitment)
        || p.line_items.is_empty()
        || p.line_items.len() > 50
        || p.line_items.iter().any(|item| item.amount < 0)
        || p.line_items
            .iter()
            .try_fold(0_i64, |sum, item| sum.checked_add(item.amount))
            != Some(p.gross_total)
    {
        return result;
    }
    result.payment.gross_amount = Some(p.gross_total);
    result.payment.discount_amount = Some(p.discount_total);
    result.payment.line_items_json = serde_json::to_string(&p.line_items).ok();
    result
}

fn summary_items(items: &[LineItem], limit: usize) -> Vec<LineItem> {
    if items.len() <= limit {
        return items.to_vec();
    }
    let mut summary = items[..limit - 1].to_vec();
    summary.push(LineItem {
        label: "Other items".into(),
        amount: items[limit - 1..].iter().map(|item| item.amount).sum(),
    });
    summary
}

fn invoice_line_items(context: &PaymentDocumentContext) -> Vec<LineItem> {
    context
        .payment
        .line_items_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<Vec<LineItem>>(value).ok())
        .map(|items| {
            items
                .into_iter()
                .filter(|item| item.amount >= 0)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| {
            vec![LineItem {
                label: pretty_payment_type(&context.payment.payment_type),
                amount: context
                    .payment
                    .gross_amount
                    .unwrap_or(context.payment.amount),
            }]
        })
}

fn student_label(context: &PaymentDocumentContext) -> String {
    if context.student_names.is_empty() {
        "Student application".to_string()
    } else {
        context.student_names.join(", ")
    }
}

fn payment_method_label(context: &PaymentDocumentContext) -> String {
    match context.payment.payment_method.as_deref() {
        Some("manual_transfer") => "Bank Transfer".to_string(),
        Some("doku") => "Online Payment (DOKU)".to_string(),
        Some("xendit") => "Online Payment (Xendit)".to_string(),
        Some(value) if !value.trim().is_empty() => title_case(value),
        _ if context.payment.hosted_invoice_url.is_some() => "Online Payment".to_string(),
        _ => "Parent Portal".to_string(),
    }
}

fn pretty_payment_type(value: &str) -> String {
    match value {
        "application_fee" => "Application Fee".to_string(),
        "enrolment_fee" | "offer_due_now" => "Enrolment Fee".to_string(),
        other => title_case(other),
    }
}

fn title_case(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_date(value: Option<&str>) -> String {
    let Ok(parsed) = value.unwrap_or_default().parse::<DateTime<Utc>>() else {
        return "Date unavailable".to_string();
    };
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    format!(
        "{} {}, {}",
        MONTHS[parsed.month0() as usize],
        parsed.day(),
        parsed.year()
    )
}

fn format_currency(amount: i64, currency: &str) -> String {
    if currency.eq_ignore_ascii_case("IDR") || currency.trim().is_empty() {
        format!("Rp {}", format_money(amount))
    } else {
        format!("{} {}", currency.to_uppercase(), format_money(amount))
    }
}

fn format_money(amount: i64) -> String {
    let negative = amount < 0;
    let digits = amount.saturating_abs().to_string();
    let mut grouped = String::new();
    for (index, character) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(character);
    }
    let formatted: String = grouped.chars().rev().collect();
    if negative {
        format!("-{formatted}")
    } else {
        formatted
    }
}

fn fit(value: &str, max: usize) -> String {
    let clean = value.trim();
    if clean.chars().count() <= max {
        return clean.to_string();
    }
    let mut shortened = clean
        .chars()
        .take(max.saturating_sub(3))
        .collect::<String>();
    shortened.push_str("...");
    shortened
}

fn wrap(value: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in value.split_whitespace() {
        if !current.is_empty() && current.chars().count() + word.chars().count() + 1 > width {
            lines.push(current);
            current = String::new();
            if lines.len() == max_lines {
                break;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if lines.len() < max_lines && !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[derive(Clone, Copy)]
struct SchoolBrand {
    code: &'static str,
    heading: &'static str,
    name: &'static str,
    accent: Color,
}

fn school_brand(value: &str) -> SchoolBrand {
    let normalized = value
        .trim()
        .trim_start_matches("SCH-")
        .trim_start_matches("SCHOOL-")
        .to_uppercase();
    match normalized.as_str() {
        "IIHS" => SchoolBrand {
            code: "IIHS",
            heading: "SMA - IIHS",
            name: "International Islamic High School",
            accent: Color(0.18, 0.40, 0.20),
        },
        "IIBS" => SchoolBrand {
            code: "IIBS",
            heading: "IIBS",
            name: "International Islamic Boarding School",
            accent: Color(0.12, 0.34, 0.29),
        },
        _ => SchoolBrand {
            code: "IISS",
            heading: "SMP - IISS",
            name: "International Islamic Secondary School",
            accent: Color(0.96, 0.49, 0.08),
        },
    }
}

fn escape_pdf_text(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '(' => output.push_str("\\("),
            ')' => output.push_str("\\)"),
            '–' | '—' | '−' => output.push('-'),
            '×' => output.push('x'),
            value if value.is_ascii() => output.push(value),
            _ => output.push('?'),
        }
    }
    output
}

fn build_pdf(stream: &[u8], background: Option<TemplateBackground>, details: &[String]) -> Vec<u8> {
    let mut content = Vec::new();
    if background.is_some() {
        content.extend_from_slice(b"q 595 0 0 842 0 0 cm /Bg Do Q\n");
    }
    content.extend_from_slice(stream);

    let resources = if background.is_some() {
        "/Resources << /Font << /F1 5 0 R /F2 6 0 R >> /XObject << /Bg 7 0 R >> >>"
    } else {
        "/Resources << /Font << /F1 5 0 R /F2 6 0 R >> >>"
    };
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] {resources} /Contents 4 0 R >>"
        )
        .into_bytes(),
        [
            format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(),
            content.as_slice(),
            b"\nendstream",
        ]
        .concat(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ];
    if let Some(background) = background {
        objects.push(
            [
                format!(
                    "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
                    background.width,
                    background.height,
                    background.jpeg.len()
                )
                .as_bytes(),
                background.jpeg,
                b"\nendstream",
            ]
            .concat(),
        );
    }
    let mut kids = String::from("3 0 R");
    for detail in details {
        let page_id = objects.len() + 1;
        kids.push_str(&format!(" {page_id} 0 R"));
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents {} 0 R >>", page_id + 1).into_bytes());
        objects.push(
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                detail.len(),
                detail
            )
            .into_bytes(),
        );
    }
    objects[1] = format!(
        "<< /Type /Pages /Kids [{kids}] /Count {} >>",
        1 + details.len()
    )
    .into_bytes();
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0usize];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::payment::Payment;

    fn context(school: &str, status: &str) -> PaymentDocumentContext {
        PaymentDocumentContext {
            pricing_snapshot_json: None,
            payment: Payment {
                payment_id: "PAY-DEMO-123".into(),
                tenant_id: "TENANT-001".into(),
                payment_type: "enrolment_fee".into(),
                status: status.into(),
                amount: 67_500_000,
                gross_amount: Some(75_000_000),
                discount_amount: Some(7_500_000),
                net_amount: Some(67_500_000),
                promotion_code: Some("EARLY10".into()),
                promotion_rule_id: None,
                promotion_snapshot_json: None,
                line_items_json: Some(r#"[{"label":"Building Fee","amount":75000000},{"label":"Early-bird discount","amount":-7500000}]"#.into()),
                currency: "IDR".into(),
                payment_method: Some("manual_transfer".into()),
                gateway_ref: None,
                invoice_ref: None,
                hosted_invoice_url: None,
                receipt_ref: Some("RCPT-DEMO-123".into()),
                paid_at: Some("2026-09-15T02:00:00Z".into()),
                expires_at: None,
                lead_id: Some("LEAD-DEMO".into()),
                manual_reference: Some("TWSI-DEMO".into()),
                amount_submitted: Some(67_500_000),
                amount_verified: Some(67_500_000),
                short_amount: Some(0),
                overpaid_amount: Some(0),
                manual_bank_account_id: Some("BANK-1".into()),
                bank_name: Some("Bank Mandiri".into()),
                bank_account_name: Some("PT TWSI Indonesia Jaya".into()),
                bank_account_number: Some("156-00-27876210".into()),
                review_note: None,
                rejection_reason: None,
                reviewed_by: None,
                reviewed_at: None,
            },
            parent_name: "Example Parent".into(),
            parent_email: "parent@example.test".into(),
            parent_location: Some("Jakarta".into()),
            school_code: school.into(),
            student_names: vec!["Example Student".into()],
            created_at: Some("2026-09-12T02:00:00Z".into()),
        }
    }

    #[test]
    fn approved_default_invoices_have_four_items_and_one_page() {
        for (school, levy, discount) in [
            ("IISS", "Capital Levy", 30_000_000),
            ("IIHS", "Building Fee", 37_500_000),
        ] {
            let mut c = context(school, "paid");
            c.payment.amount = 105_000_000 - discount;
            c.pricing_snapshot_json = Some(serde_json::json!({"currency":"IDR","lineItems":[
                {"label":levy,"amount":75_000_000},
                {"label":"Monthly Fee - July 2027","amount":7_000_000},
                {"label":"Uniform","amount":15_000_000},
                {"label":"Book","amount":8_000_000}
            ],"grossTotal":105_000_000,"discountTotal":discount,"netCommitment":c.payment.amount,"amountDueNow":c.payment.amount}).to_string());
            for kind in [PaymentDocumentKind::Invoice, PaymentDocumentKind::Receipt] {
                let pdf = render_payment_document(&c, kind);
                let text = String::from_utf8_lossy(&pdf);
                assert!(text.contains("/Count 1"));
                assert!(!text.contains("Other items"));
                assert!(text.contains("Monthly Fee - July 2027"));
                if let Some(directory) = std::env::var_os("WRITE_PAYMENT_PDF_FIXTURES") {
                    let directory = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let suffix = if kind == PaymentDocumentKind::Invoice {
                        "invoice"
                    } else {
                        "receipt"
                    };
                    std::fs::write(
                        directory.join(format!("{}-default-{suffix}.pdf", school.to_lowercase())),
                        pdf,
                    )
                    .unwrap();
                }
            }
        }
    }
    #[test]
    fn renders_school_specific_invoice_and_verified_receipt() {
        let invoice = render_payment_document(
            &context("SCH-IIHS", "pending"),
            PaymentDocumentKind::Invoice,
        );
        let receipt =
            render_payment_document(&context("SCH-IISS", "paid"), PaymentDocumentKind::Receipt);
        if let Some(directory) = std::env::var_os("WRITE_PAYMENT_PDF_FIXTURES") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).expect("create PDF fixture directory");
            std::fs::write(directory.join("iihs-invoice.pdf"), &invoice)
                .expect("write IIHS invoice fixture");
            std::fs::write(directory.join("iiss-receipt.pdf"), &receipt)
                .expect("write IISS receipt fixture");
            std::fs::write(
                directory.join("iiss-invoice.pdf"),
                render_payment_document(
                    &context("SCH-IISS", "pending"),
                    PaymentDocumentKind::Invoice,
                ),
            )
            .expect("write IISS invoice fixture");
            std::fs::write(
                directory.join("iihs-receipt.pdf"),
                render_payment_document(&context("SCH-IIHS", "paid"), PaymentDocumentKind::Receipt),
            )
            .expect("write IIHS receipt fixture");
        }
        assert!(invoice.starts_with(b"%PDF-1.4"));
        assert!(receipt.starts_with(b"%PDF-1.4"));
        let invoice_text = String::from_utf8_lossy(&invoice);
        let receipt_text = String::from_utf8_lossy(&receipt);
        assert!(invoice_text.contains("/Subtype /Image"));
        assert!(invoice_text.contains("/Bg Do"));
        assert!(invoice_text.contains("Example Parent"));
        assert!(invoice_text.contains("67.500.000"));
        assert!(receipt_text.contains("/Subtype /Image"));
        assert!(receipt_text.contains("Example Parent"));
        assert!(receipt_text.contains("RCPT-DEMO-123"));
    }

    #[test]
    fn overflow_is_summarized_on_one_page_and_partial_payments_stay_due_now() {
        let mut c = context("SCH-IISS", "paid");
        let items: Vec<_> = (0..15).map(|index| serde_json::json!({"label": format!("Configured fee item {}", index + 1), "amount": 1_000_000})).collect();
        c.payment.amount = 14_000_000;
        c.pricing_snapshot_json = Some(serde_json::json!({"currency":"IDR","lineItems":items,"grossTotal":15_000_000,"discountTotal":1_000_000,"netCommitment":14_000_000,"amountDueNow":14_000_000}).to_string());
        let hydrated = hydrate_offer_items(&c);
        assert_eq!(invoice_line_items(&hydrated).len(), 15);
        assert_eq!(hydrated.payment.gross_amount, Some(15_000_000));
        let pdf = render_payment_document(&c, PaymentDocumentKind::Invoice);
        let text = String::from_utf8_lossy(&pdf);
        assert!(!text.contains("Fee item details"));
        assert!(text.contains("/Count 1"));
        assert!(text.contains("Other items"));
        if let Some(directory) = std::env::var_os("WRITE_PAYMENT_PDF_FIXTURES") {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                std::path::PathBuf::from(directory).join("item-details.pdf"),
                pdf,
            )
            .unwrap();
        }
        c.payment.line_items_json = None;
        c.payment.gross_amount = None;
        c.payment.amount = 5_000_000;
        assert_eq!(
            invoice_line_items(&hydrate_offer_items(&c))[0].amount,
            5_000_000
        );
    }

    #[test]
    fn formats_idr_and_sanitizes_pdf_text() {
        // AFM widths keep the same right edge for differently sized amounts.
        assert!((estimated_text_width("1.000.000", 9.0, true) - 40.032).abs() < 0.001);
        assert!((estimated_text_width("12.000.000", 9.0, true) - 45.036).abs() < 0.001);
        assert_eq!(format_currency(75_000_000, "IDR"), "Rp 75.000.000");
        assert_eq!(escape_pdf_text("Fee — (term)"), "Fee - \\(term\\)");
    }

    #[test]
    fn receipt_requires_a_verified_paid_status() {
        assert!(is_document_available(
            PaymentDocumentKind::Invoice,
            "pending"
        ));
        assert!(is_document_available(PaymentDocumentKind::Receipt, "paid"));
        assert!(!is_document_available(
            PaymentDocumentKind::Receipt,
            "pending_verification"
        ));
    }
}
