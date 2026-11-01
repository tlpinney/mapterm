//! Map icons as single terminal cells. A style's `icon-image` names an icon in its sprite
//! (`hospital`, `airport_11`, `circle_11_black`); mapterm can't draw sprites, so it shows a
//! one-cell symbol for the icons it knows: OpenMapTiles POI classes, Maki icon names, and
//! Protomaps POI kinds. Only symbols that are one cell wide in every terminal are used (no
//! emoji, whose width varies), so labels beside them line up.

/// The base name of a sprite icon: `airport_11` -> `airport`, `circle_11_black` -> `circle`,
/// `maki-cafe` -> `cafe`.
fn base(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let n = n.strip_prefix("maki-").or_else(|| n.strip_prefix("poi-")).unwrap_or(&n).to_string();
    let parts: Vec<&str> = n.split(['_', '-']).collect();
    // Drop a size and anything after it ("_11", "_15_black").
    match parts.iter().position(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
        Some(0) | None => n.replace('-', "_"),
        Some(i) => parts[..i].join("_"),
    }
}

/// The glyph for a sprite icon name, if mapterm knows one.
pub fn glyph(name: &str) -> Option<&'static str> {
    Some(match base(name).as_str() {
        "hospital" | "doctor" | "doctors" | "clinic" | "dentist" | "veterinary" | "emergency" => "✚",
        "pharmacy" | "chemist" => "℞",
        "airport" | "aerodrome" | "airfield" | "heliport" => "✈",
        "rail" | "railway" | "rail_light" | "train" | "station" | "train_station" | "halt" => "Ⓡ",
        "rail_metro" | "subway" | "metro" | "subway_entrance" => "Ⓜ",
        "bus" | "bus_stop" | "bus_station" => "Ⓑ",
        "tram" | "tram_stop" => "Ⓣ",
        "ferry" | "ferry_terminal" | "harbor" | "harbour" | "marina" => "≋",
        "parking" | "parking_garage" | "car_parking" => "Ⓟ",
        "fuel" | "gas" | "gas_station" | "charging_station" => "⊕",
        "restaurant" | "fast_food" | "food_court" | "bakery" | "ice_cream" | "restaurant_noodle" | "restaurant_pizza" => "⋔",
        "cafe" | "coffee" => "◡",
        "bar" | "pub" | "beer" | "biergarten" | "alcohol_shop" | "wine" | "nightclub" => "⊻",
        "lodging" | "hotel" | "motel" | "hostel" | "guest_house" | "campsite" | "camp_site" => "Ⓗ",
        "shop" | "grocery" | "supermarket" | "convenience" | "clothing_store" | "mall" | "department_store" | "marketplace" | "florist"
        | "furniture" | "hardware" | "jewelry_store" | "shoe" | "shoes" | "mobile_phone" | "laundry" => "◈",
        "bank" | "atm" | "bureau_de_change" | "money" => "$",
        "post" | "post_office" | "post_box" => "✉",
        "school" | "college" | "university" | "kindergarten" | "education" => "✎",
        "library" | "books" | "book_store" | "bookstore" => "▤",
        "museum" | "art_gallery" | "gallery" | "arts_centre" => "♖",
        "monument" | "memorial" | "castle" | "ruins" | "historic" => "♜",
        "attraction" | "viewpoint" | "information" | "tourism" => "★",
        "place_of_worship" | "religious_christian" | "church" | "cathedral" | "chapel" => "✝",
        "religious_muslim" | "mosque" => "☪",
        "religious_jewish" | "synagogue" => "✡",
        "town_hall" | "townhall" | "courthouse" | "embassy" | "government" => "⚖",
        "police" => "✪",
        "fire_station" => "♨",
        "park" | "garden" | "nature_reserve" | "national_park" | "picnic_site" => "♣",
        "playground" | "zoo" | "theme_park" | "aquarium" => "☺",
        "stadium" | "sports_centre" | "pitch" | "golf" | "swimming" | "swimming_pool" | "fitness_centre" | "fitness" => "◎",
        "theatre" | "cinema" | "music" | "concert_hall" => "♫",
        "toilets" | "toilet" | "drinking_water" => "Ⓦ",
        "peak" | "mountain" | "volcano" | "saddle" => "▲",
        "circle" | "dot" | "dot_black" | "dot_white" => "•",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn names_and_sizes() {
        assert_eq!(base("airport_11"), "airport");
        assert_eq!(base("circle_11_black"), "circle");
        assert_eq!(base("maki-fast-food"), "fast_food");
        assert_eq!(base("place_of_worship"), "place_of_worship");
        assert_eq!(glyph("airport_11"), Some("✈"));
        assert_eq!(glyph("hospital"), Some("✚"));
        assert_eq!(glyph("circle_11_black"), Some("•"));
        assert_eq!(glyph("us-interstate_3"), None, "highway shields aren't point icons");
        assert_eq!(glyph(""), None);
    }

    #[test]
    fn every_glyph_is_one_cell() {
        let names = [
            "hospital", "pharmacy", "airport", "rail", "subway", "bus", "tram", "ferry", "parking", "fuel", "restaurant", "cafe", "bar", "lodging", "shop",
            "bank", "post", "school", "library", "museum", "monument", "attraction", "place_of_worship", "religious_muslim", "religious_jewish",
            "town_hall", "police", "fire_station", "park", "playground", "stadium", "theatre", "toilets", "peak", "circle",
        ];
        for n in names {
            let g = glyph(n).unwrap_or_else(|| panic!("{n}"));
            assert_eq!(g.width(), 1, "{n} -> {g}");
            assert_eq!(g.chars().count(), 1, "{n} -> {g}");
        }
    }
}
