//! What a great storm is called where you are, from the time zone's name: a typhoon in the
//! north-west Pacific, a cyclone round the Indian Ocean and in the South Pacific, a hurricane
//! everywhere else and wherever the zone is unknown.

use std::path::Path;

use argonauts_core::behavior_world::StormWord;

const TYPHOON: &[&str] = &[
    "Asia/Tokyo",
    "Asia/Seoul",
    "Asia/Pyongyang",
    "Asia/Shanghai",
    "Asia/Chongqing",
    "Asia/Chungking",
    "Asia/Harbin",
    "Asia/Urumqi",
    "Asia/Hong_Kong",
    "Asia/Macau",
    "Asia/Taipei",
    "Asia/Manila",
    "Asia/Ho_Chi_Minh",
    "Asia/Saigon",
    "Asia/Hanoi",
    "Asia/Bangkok",
    "Asia/Phnom_Penh",
    "Asia/Vientiane",
    "Asia/Brunei",
    "Asia/Kuching",
    "Asia/Makassar",
    "Pacific/Guam",
    "Pacific/Saipan",
    "Pacific/Palau",
    "Pacific/Chuuk",
    "Pacific/Truk",
    "Pacific/Pohnpei",
    "Pacific/Ponape",
    "Pacific/Kosrae",
    "Pacific/Majuro",
    "Pacific/Kwajalein",
    "Japan",
    "ROK",
    "ROC",
    "PRC",
    "Hongkong",
];

const CYCLONE: &[&str] = &[
    "Asia/Kolkata",
    "Asia/Calcutta",
    "Asia/Dhaka",
    "Asia/Dacca",
    "Asia/Karachi",
    "Asia/Colombo",
    "Asia/Yangon",
    "Asia/Rangoon",
    "Asia/Kathmandu",
    "Asia/Katmandu",
    "Asia/Thimphu",
    "Asia/Muscat",
    "Asia/Dubai",
    "Asia/Aden",
    "Asia/Jakarta",
    "Asia/Pontianak",
    "Asia/Jayapura",
    "Asia/Dili",
    "Pacific/Auckland",
    "Pacific/Chatham",
    "Pacific/Fiji",
    "Pacific/Noumea",
    "Pacific/Port_Moresby",
    "Pacific/Bougainville",
    "Pacific/Efate",
    "Pacific/Tongatapu",
    "Pacific/Apia",
    "Pacific/Guadalcanal",
    "Pacific/Norfolk",
    "Pacific/Niue",
    "Pacific/Rarotonga",
    "Pacific/Tahiti",
    "Pacific/Wallis",
    "Pacific/Funafuti",
    "Pacific/Tarawa",
    "Pacific/Nauru",
    "Pacific/Fakaofo",
    "Africa/Maputo",
    "Africa/Dar_es_Salaam",
    "Africa/Nairobi",
    "Africa/Mogadishu",
    "Africa/Djibouti",
    "NZ",
];

/// Every zone of these regions is a cyclone's.
const CYCLONE_REGIONS: &[&str] = &["Indian/", "Australia/"];

pub(crate) fn word(zone: Option<&str>) -> StormWord {
    let Some(zone) = zone else {
        return StormWord::Hurricane;
    };
    if TYPHOON.contains(&zone) {
        StormWord::Typhoon
    } else if CYCLONE.contains(&zone)
        || CYCLONE_REGIONS.iter().any(|region| {
            zone.strip_prefix(region)
                .is_some_and(|rest| !rest.is_empty())
        })
    {
        StormWord::Cyclone
    } else {
        StormWord::Hurricane
    }
}

/// The time zone's name, such as `Asia/Taipei`: `TZ` when it names one, else where
/// `/etc/localtime` links to.
pub(crate) fn zone() -> Option<String> {
    let link = std::fs::read_link("/etc/localtime").ok();
    zone_of(std::env::var("TZ").ok().as_deref(), link.as_deref())
}

fn zone_of(tz: Option<&str>, localtime: Option<&Path>) -> Option<String> {
    if let Some(tz) = tz.map(|tz| tz.trim_start_matches(':').trim())
        && !tz.is_empty()
    {
        return named(Path::new(tz)).or_else(|| looks_like_a_name(tz).then(|| tz.to_owned()));
    }
    localtime.and_then(named)
}

/// The zone a TZif file's path names: what follows `zoneinfo/`.
fn named(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let (_, name) = text.split_once("zoneinfo/")?;
    Some(name.trim_start_matches('/').to_owned()).filter(|name| !name.is_empty())
}

/// `Asia/Taipei` or `Japan` rather than a POSIX rule such as `EST5EDT` or `<+08>-8`, which
/// names no place.
fn looks_like_a_name(tz: &str) -> bool {
    !tz.starts_with('/')
        && tz
            .chars()
            .all(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_storm_is_named_as_the_zone_s_region_names_it() {
        for (zone, said) in [
            (Some("Asia/Taipei"), StormWord::Typhoon),
            (Some("Asia/Tokyo"), StormWord::Typhoon),
            (Some("Pacific/Guam"), StormWord::Typhoon),
            (Some("Asia/Kolkata"), StormWord::Cyclone),
            (Some("Australia/Sydney"), StormWord::Cyclone),
            (Some("Indian/Mauritius"), StormWord::Cyclone),
            (Some("Pacific/Auckland"), StormWord::Cyclone),
            (Some("America/New_York"), StormWord::Hurricane),
            (Some("Europe/London"), StormWord::Hurricane),
            (Some("Pacific/Honolulu"), StormWord::Hurricane),
            (Some("Australia/"), StormWord::Hurricane),
            (None, StormWord::Hurricane),
        ] {
            assert_eq!(word(zone), said, "{zone:?}");
        }
    }

    #[test]
    fn the_zone_is_tz_s_name_else_where_localtime_links() {
        let link = Path::new("/var/db/timezone/zoneinfo/Asia/Taipei");
        assert_eq!(zone_of(None, Some(link)).as_deref(), Some("Asia/Taipei"));
        assert_eq!(
            zone_of(Some(":Asia/Tokyo"), Some(link)).as_deref(),
            Some("Asia/Tokyo")
        );
        assert_eq!(
            zone_of(Some("/usr/share/zoneinfo/Australia/Perth"), None).as_deref(),
            Some("Australia/Perth")
        );
        assert_eq!(zone_of(Some("EST5EDT,M3.2.0,M11.1.0"), Some(link)), None);
        assert_eq!(
            zone_of(Some(""), Some(link)).as_deref(),
            Some("Asia/Taipei")
        );
        assert_eq!(zone_of(None, Some(Path::new("/etc/UTC"))), None);
        assert_eq!(zone_of(None, None), None);
    }
}
