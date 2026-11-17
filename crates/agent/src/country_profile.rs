//! Where a call is: the region of the SIM that took it (else of the language
//! setting), its candidate dialects ([`crate::dialect`]) and the few local rules
//! that go into that call's prompt (scenario plan, module 6): where couriers
//! leave parcels, times and numbers said the local way, and the local scams.
//!
//! Decided from memory before the call is picked up (the ring prewarm has the
//! SIM): a table lookup, no I/O. Each region's part is one short paragraph,
//! only that region's goes into a prompt ([`crate::prompts::scope_to_region`]).
//! Instructions are in the prompt's own instruction language: Chinese for the
//! two Chinese settings, English for the others (as every other part of those
//! prompts, [`crate::prompts`]); local words stay in the local script.

use crate::dialect::Dialect;
use crate::locale::Lang;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    Tw,
    HkMo,
    Cn,
    MySg,
    Us,
    Ca,
    GbIe,
    AuNz,
    In,
    Jp,
    Kr,
    /// Spain.
    EsEu,
    /// Mexico, Central and South America, the Caribbean (but Argentina, Uruguay).
    EsAm,
    /// Argentina, Uruguay.
    EsRp,
    Fr,
    Be,
    De,
    At,
    Ch,
    Br,
    Pt,
}

/// The region of a SIM country (ISO 3166 alpha-2, any case); None for a
/// country without one.
pub fn of_sim(iso: &str) -> Option<Region> {
    let iso = iso.trim().to_ascii_lowercase();
    Some(match iso.as_str() {
        "tw" => Region::Tw,
        "hk" | "mo" => Region::HkMo,
        "cn" => Region::Cn,
        "my" | "sg" => Region::MySg,
        "us" | "ag" | "ai" | "as" | "bb" | "bm" | "bs" | "dm" | "gd" | "gu" | "jm" | "kn" | "ky" | "lc" | "mp" | "ms"
        | "sx" | "tc" | "tt" | "vc" | "vg" | "vi" => Region::Us,
        "ca" => Region::Ca,
        "gb" | "ie" | "im" | "je" | "gg" => Region::GbIe,
        "au" | "nz" => Region::AuNz,
        "in" => Region::In,
        "jp" => Region::Jp,
        "kr" => Region::Kr,
        "es" => Region::EsEu,
        "mx" | "co" | "cl" | "pe" | "ve" | "ec" | "bo" | "py" | "gt" | "hn" | "sv" | "ni" | "cr" | "pa" | "cu" | "do"
        | "pr" => Region::EsAm,
        "ar" | "uy" => Region::EsRp,
        "fr" | "re" | "yt" | "gp" | "bl" | "mf" | "mq" | "gf" | "pm" | "nc" | "pf" | "wf" | "mc" | "lu" => Region::Fr,
        "be" => Region::Be,
        "de" => Region::De,
        "at" => Region::At,
        "ch" | "li" => Region::Ch,
        "br" => Region::Br,
        "pt" => Region::Pt,
        _ => return None,
    })
}

/// The region a language setting stands for when the SIM is not known.
pub fn of_lang(lang: Lang) -> Region {
    match lang {
        Lang::ZhTw => Region::Tw,
        Lang::ZhCn => Region::Cn,
        Lang::En => Region::Us,
        Lang::Ja => Region::Jp,
        Lang::Ko => Region::Kr,
        Lang::Es => Region::EsAm,
        Lang::Fr => Region::Fr,
        Lang::De => Region::De,
        Lang::Pt => Region::Br,
    }
}

/// A call's region: its SIM's (a SIM of a country without one: none), else the
/// language setting's.
pub fn resolve(sim: Option<&str>, lang: Lang) -> Option<Region> {
    match sim.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => of_sim(s),
        None => Some(of_lang(lang)),
    }
}

/// The candidate dialects and local ways of speaking of a region (two or three
/// at most; the general English and the auto-language rule cover the rest).
pub fn dialects(region: Region) -> &'static [Dialect] {
    use Dialect::*;
    match region {
        Region::Tw => &[Hokkien, Hakka],
        Region::HkMo => &[Cantonese],
        Region::Cn => &[Cantonese, Hokkien, Sichuanese],
        Region::MySg => &[Hokkien, Cantonese, Singlish],
        Region::Us => &[],
        Region::Ca => &[Quebec],
        Region::GbIe => &[BritishRegional],
        Region::AuNz => &[Aussie],
        Region::In => &[IndianEnglish],
        Region::Jp => &[Kansai],
        Region::Kr => &[Gyeongsang],
        Region::EsEu => &[PeninsularSpanish],
        Region::EsAm => &[LatinSpanish],
        Region::EsRp => &[Rioplatense],
        Region::Fr | Region::Be => &[BelgianSwissFrench],
        Region::De => &[Bavarian, SwissGerman],
        Region::At => &[Bavarian],
        Region::Ch => &[SwissGerman],
        Region::Br => &[BrazilianPortuguese],
        Region::Pt => &[EuropeanPortuguese],
    }
}

/// The kinds of local rule a region has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Where couriers leave a parcel.
    Delivery,
    /// Times and numbers said the local way, to read back.
    Readback,
    /// Scams of the place, as the institutions they claim to be.
    Fraud,
}

/// (Traditional Chinese, English) of a region's rule; None = the region has none of that kind.
fn text(region: Region, kind: Kind) -> Option<(&'static str, &'static str)> {
    use Kind::*;
    use Region::*;
    const DE_PARCEL: (&str, &str) = ("包裹交給鄰居或 Packstation", "Parcels: beim Nachbarn or a Packstation");
    const DE_FRAUD: (&str, &str) = ("要 TAN 碼、補繳海關費的都是詐騙", "a TAN or customs fees asked for are scams");
    const FR_PARCEL: (&str, &str) = ("包裹放 point relais 或交給鄰居", "Parcels: a point relais or a neighbour");
    const FR_FRAUD: (&str, &str) = ("要簡訊驗證碼、補繳包裹費的都是詐騙", "SMS codes or parcel fees asked for are scams");
    const ES_FRAUD: (&str, &str) = ("要你付海關費、給簡訊驗證碼的都是詐騙", "customs fees or SMS codes asked for are scams");
    const US_PARCEL: (&str, &str) = ("包裹請他放 front porch 或門口，不要編門禁密碼", "Parcels: front porch or door, no made-up gate code");
    Some(match (region, kind) {
        (Tw, Delivery) => ("快遞請他放管理室或門口", "Parcels: the building's management desk or the door"),
        (Tw, Fraud) => (
            "說要解除分期、去 ATM 操作、做賣場金流認證的都是詐騙",
            "cancelling an instalment plan, ATM steps or a shop's payment check are scams",
        ),
        (HkMo, Delivery) => ("快遞請他放管理處或門口", "Parcels: the building's management office or the door"),
        (HkMo, Fraud) => (
            "自稱入境處、公安或速遞公司要你轉帳、給驗證碼的都是詐騙",
            "\"Immigration\", mainland police or couriers asking for transfers or codes are scams",
        ),
        (Cn, Delivery) => ("快遞放快遞櫃、驛站或門口", "Parcels: a parcel locker, pickup station or the door"),
        (Cn, Fraud) => (
            "自稱公檢法要你轉到安全帳戶、刷流水的都是詐騙",
            "\"police or prosecutors\" moving money to a \"safe account\" are scams",
        ),
        (MySg, Delivery) => ("快遞放門口或保安亭", "Parcels: the door or the guardhouse"),
        (MySg, Fraud) => (
            "自稱警察、銀行或政府部門要你轉帳的都是詐騙",
            "\"police\", bank or government officers asking for transfers are scams",
        ),
        (Us, Delivery) | (Ca, Delivery) => US_PARCEL,
        (Us, Fraud) => ("拿 IRS、Medicare 威脅或要你用禮品卡付款的都是詐騙", "IRS, Medicare or gift-card demands are scams"),
        (Ca, Fraud) => ("拿 CRA 稅務威脅或要你用禮品卡付款的都是詐騙", "CRA or gift-card demands are scams"),
        (GbIe, Delivery) => ("包裹放 safe place 或交給鄰居", "Parcels: a safe place or a neighbour"),
        (GbIe, Fraud) => (
            "自稱 HMRC 或銀行要你轉到 safe account 的都是詐騙",
            "HMRC or a bank moving money to a \"safe account\" are scams",
        ),
        (AuNz, Delivery) => ("包裹請他 leave at the front door", "Parcels: leave at the front door"),
        (AuNz, Fraud) => ("自稱 ATO、銀行或 myGov 要你付款的都是詐騙", "ATO, bank or myGov payment demands are scams"),
        (In, Delivery) => ("包裹交給 security 或放門口", "Parcels: with security or at the door"),
        (In, Fraud) => ("說要更新 KYC、付包裹海關費的都是詐騙", "KYC updates and customs fees for a parcel are scams"),
        (Jp, Delivery) => (
            "包裹請他置き配、放宅配ボックス或留不在票再送",
            "Parcels: 置き配, the 宅配ボックス or a 不在票 for redelivery",
        ),
        (Jp, Fraud) => ("叫你去 ATM 領還付金的都是詐騙", "being sent to an ATM for a 還付金 refund is a scam"),
        (Kr, Delivery) => ("包裹放문 앞或경비실", "Parcels: 문 앞 or the 경비실"),
        (Kr, Fraud) => (
            "自稱檢察廳要你把錢移到安全帳戶的都是詐騙",
            "\"prosecutors\" asking to move money to a safe account are scams",
        ),
        (EsEu, Delivery) => ("包裹交給門房（conserje）或鄰居", "Parcels: the conserje or a neighbour"),
        (EsAm, Delivery) | (EsRp, Delivery) => ("包裹交給門房（portería）或鄰居", "Parcels: the portería or a neighbour"),
        (EsEu, Fraud) | (EsAm, Fraud) | (EsRp, Fraud) => ES_FRAUD,
        (Fr, Delivery) | (Be, Delivery) => FR_PARCEL,
        (Be, Readback) => ("septante=70、nonante=90，數字要複誦", "septante = 70, nonante = 90: read numbers back"),
        (Fr, Fraud) | (Be, Fraud) => FR_FRAUD,
        (De, Delivery) | (At, Delivery) | (Ch, Delivery) => DE_PARCEL,
        (De, Readback) | (At, Readback) => ("halb acht 是 7:30，複誦成 19 Uhr 30", "halb acht means 7:30: read it back as 19 Uhr 30"),
        (Ch, Readback) => ("halb acht=7:30、huitante=80，要複誦", "halb acht = 7:30, huitante = 80: read times and numbers back"),
        (De, Fraud) | (At, Fraud) | (Ch, Fraud) => DE_FRAUD,
        (Br, Delivery) => ("包裹留在 portaria", "Parcels: the portaria"),
        (Br, Fraud) => ("要你用 PIX 轉帳、付中獎手續費的都是詐騙", "PIX transfers or fees for a \"prize\" are scams"),
        (Pt, Delivery) => ("包裹留 portaria 或交給鄰居", "Parcels: the portaria or a neighbour"),
        (Pt, Fraud) => ("要你用 MB WAY 操作或給驗證碼的都是詐騙", "MB WAY steps or codes asked for are scams"),
        _ => return None,
    })
}

/// One local rule of a region in the instruction language of `lang`'s prompts,
/// without the paragraph's heading — e.g. the delivery line a scenario hint uses.
pub fn line(region: Region, kind: Kind, lang: Lang) -> Option<String> {
    let (zh, en) = text(region, kind)?;
    Some(match lang {
        Lang::ZhTw => zh.to_string(),
        Lang::ZhCn => crate::zh_convert::to_cn(zh),
        _ => en.to_string(),
    })
}

/// The region's paragraph for a prompt in `lang` ("【當地】…" / "[Local] …"), led
/// by a line break; empty when the region has no rule. `skip_delivery`: the
/// prompt says where parcels go already.
pub fn block(region: Region, lang: Lang, skip_delivery: bool) -> String {
    let kinds = [Kind::Delivery, Kind::Readback, Kind::Fraud];
    let lines: Vec<String> = kinds
        .iter()
        .filter(|k| !(skip_delivery && **k == Kind::Delivery))
        .filter_map(|k| line(region, *k, lang))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    match lang {
        Lang::ZhTw => format!("\n【當地】{}。", lines.join("；")),
        Lang::ZhCn => format!("\n【当地】{}。", lines.join("；")),
        _ => format!("\n[Local] {}.", lines.join("; ")),
    }
}

/// A word of the region's delivery rule, in the instruction language of `lang`:
/// a prompt that has it says where parcels go already (the Taiwan prompt's
/// 「管理室」).
pub fn delivery_key(region: Region, lang: Lang) -> Option<&'static str> {
    match (region, lang) {
        (Region::Tw, Lang::ZhTw) | (Region::Tw, Lang::ZhCn) => Some("管理室"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Longest region paragraph: Chinese characters, and English (about the same
    /// number of tokens).
    const BLOCK_MAX_CHARS_ZH: usize = 90;
    const BLOCK_MAX_CHARS_EN: usize = 200;

    const ALL: [Region; 21] = [
        Region::Tw,
        Region::HkMo,
        Region::Cn,
        Region::MySg,
        Region::Us,
        Region::Ca,
        Region::GbIe,
        Region::AuNz,
        Region::In,
        Region::Jp,
        Region::Kr,
        Region::EsEu,
        Region::EsAm,
        Region::EsRp,
        Region::Fr,
        Region::Be,
        Region::De,
        Region::At,
        Region::Ch,
        Region::Br,
        Region::Pt,
    ];

    #[test]
    fn every_regions_paragraph_is_short() {
        for r in ALL {
            for lang in crate::locale::ALL {
                let b = block(r, lang, false);
                let n = b.trim().chars().count();
                let max = if lang.is_zh() { BLOCK_MAX_CHARS_ZH } else { BLOCK_MAX_CHARS_EN };
                assert!(n > 0 && n <= max, "{r:?} {lang:?}: {n} {b}");
                assert!(!b.contains('{'), "{r:?} {lang:?}");
            }
            assert!(dialects(r).len() <= 3, "{r:?}");
        }
    }

    #[test]
    fn sim_first_then_language() {
        assert_eq!(resolve(Some("TW"), Lang::En), Some(Region::Tw));
        assert_eq!(resolve(Some("mo"), Lang::ZhTw), Some(Region::HkMo));
        assert_eq!(resolve(Some("sg"), Lang::ZhCn), Some(Region::MySg));
        assert_eq!(resolve(Some("uy"), Lang::Es), Some(Region::EsRp));
        assert_eq!(resolve(None, Lang::Ja), Some(Region::Jp));
        assert_eq!(resolve(Some(" "), Lang::ZhTw), Some(Region::Tw));
        // A SIM of a country without rules: none, not the language's.
        assert_eq!(resolve(Some("th"), Lang::ZhTw), None);
    }

    #[test]
    fn a_line_is_in_the_prompts_instruction_language() {
        assert_eq!(line(Region::Tw, Kind::Delivery, Lang::ZhTw).as_deref(), Some("快遞請他放管理室或門口"));
        let cn = line(Region::Cn, Kind::Fraud, Lang::ZhCn).unwrap();
        assert!(cn.contains("安全账户") || cn.contains("安全帐户"), "{cn}");
        assert!(line(Region::Jp, Kind::Delivery, Lang::Ja).unwrap().starts_with("Parcels:"));
        assert!(line(Region::De, Kind::Readback, Lang::De).unwrap().contains("19 Uhr 30"));
        assert_eq!(line(Region::Tw, Kind::Readback, Lang::ZhTw), None);
        // The prompt already says where parcels go: only the rest.
        let b = block(Region::Tw, Lang::ZhTw, true);
        assert!(!b.contains("管理室") && b.contains("ATM"), "{b}");
    }
}
