//! Embedded world-cities reference layer for the World Map.
//!
//! ratatui's canvas [`Map`](ratatui::widgets::canvas::Map) shape only draws
//! coastlines, so an offline operator gets continents with no place names. This
//! module ships a small, curated gazetteer of national capitals and major world
//! cities — compiled in as a `const` table (no I/O, no allocation, no extra
//! dependency, matching the offline ethos of [`crate::zones::demo`]) — that the
//! renderer plots as a dim layer beneath the operator/peer markers.
//!
//! Each city carries a [`label_span`](City::label_span): the widest viewport
//! (degrees of longitude across the width) at which its *name* is drawn. The dot
//! is always plotted when in view; labels reveal progressively as the operator
//! zooms in. At the whole-globe view only the dots show (names there would just
//! overprint each other); the first zoom brings in the megacity anchors, a
//! regional zoom the capitals, and a close zoom the remaining large cities. This
//! is the same declutter-by-zoom trick paper atlases use.
//!
//! The set is deliberately partial — enough to orient the map worldwide, not a
//! complete gazetteer — and makes no political claim; contested coordinates are
//! given conservatively.

/// What a plotted place represents — drives its glyph in the UI so the two read
/// apart even stripped to monochrome (a capital ring vs. a city dot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CityKind {
    /// A national capital.
    Capital,
    /// A major (non-capital) city.
    Major,
}

/// One entry in the embedded gazetteer: a place name, where it sits, what it is,
/// and how far out its label survives. Coordinates are decimal degrees
/// (north/east positive), already within range so no normalisation is needed.
#[derive(Clone, Copy, Debug)]
pub struct City {
    /// Place name, drawn next to the dot when zoomed in past `label_span`.
    pub name: &'static str,
    /// Latitude in degrees, north positive.
    pub lat: f64,
    /// Longitude in degrees, east positive.
    pub lon: f64,
    /// Capital vs. major city (glyph selection).
    pub kind: CityKind,
    /// Widest viewport span (degrees of longitude) at which the name is drawn;
    /// larger = labelled when more zoomed out. The dot itself ignores this.
    pub label_span: f64,
}

use CityKind::{Capital, Major};

/// Megacity anchors — the first names to appear as the operator zooms in from
/// the globe (just under the 360° default so the globe view stays dots-only).
const GLOBAL: f64 = 220.0;
/// Capitals / large cities labelled at a regional zoom.
const REGION: f64 = 100.0;
/// Smaller cities labelled only once zoomed in close.
const LOCAL: f64 = 45.0;

/// Build a `City` tersely: `c(name, lat, lon, kind, span)`.
const fn c(name: &'static str, lat: f64, lon: f64, kind: CityKind, label_span: f64) -> City {
    City {
        name,
        lat,
        lon,
        kind,
        label_span,
    }
}

/// The embedded gazetteer. Curated for worldwide orientation, not completeness.
pub const CITIES: &[City] = &[
    // --- Megacities / global anchors (labelled at any zoom) ---
    c("London", 51.51, -0.13, Capital, GLOBAL),
    c("Paris", 48.85, 2.35, Capital, GLOBAL),
    c("Moscow", 55.76, 37.62, Capital, GLOBAL),
    c("Berlin", 52.52, 13.40, Capital, GLOBAL),
    c("Istanbul", 41.01, 28.98, Major, GLOBAL),
    c("Cairo", 30.04, 31.24, Capital, GLOBAL),
    c("Lagos", 6.52, 3.38, Major, GLOBAL),
    c("Delhi", 28.61, 77.21, Capital, GLOBAL),
    c("Mumbai", 19.08, 72.88, Major, GLOBAL),
    c("Beijing", 39.90, 116.41, Capital, GLOBAL),
    c("Shanghai", 31.23, 121.47, Major, GLOBAL),
    c("Tokyo", 35.68, 139.69, Capital, GLOBAL),
    c("Seoul", 37.57, 126.98, Capital, GLOBAL),
    c("Jakarta", -6.21, 106.85, Capital, GLOBAL),
    c("Singapore", 1.35, 103.82, Capital, GLOBAL),
    c("Sydney", -33.87, 151.21, Major, GLOBAL),
    c("New York", 40.71, -74.01, Major, GLOBAL),
    c("Los Angeles", 34.05, -118.24, Major, GLOBAL),
    c("Mexico City", 19.43, -99.13, Capital, GLOBAL),
    c("Sao Paulo", -23.55, -46.63, Major, GLOBAL),
    // --- Europe ---
    c("Madrid", 40.42, -3.70, Capital, REGION),
    c("Lisbon", 38.72, -9.14, Capital, REGION),
    c("Rome", 41.90, 12.50, Capital, REGION),
    c("Amsterdam", 52.37, 4.90, Capital, REGION),
    c("Brussels", 50.85, 4.35, Capital, REGION),
    c("Bern", 46.95, 7.45, Capital, REGION),
    c("Vienna", 48.21, 16.37, Capital, REGION),
    c("Warsaw", 52.23, 21.01, Capital, REGION),
    c("Prague", 50.08, 14.44, Capital, REGION),
    c("Budapest", 47.50, 19.04, Capital, REGION),
    c("Bucharest", 44.43, 26.10, Capital, REGION),
    c("Belgrade", 44.79, 20.45, Capital, REGION),
    c("Kyiv", 50.45, 30.52, Capital, REGION),
    c("Stockholm", 59.33, 18.07, Capital, REGION),
    c("Oslo", 59.91, 10.75, Capital, REGION),
    c("Helsinki", 60.17, 24.94, Capital, REGION),
    c("Copenhagen", 55.68, 12.57, Capital, REGION),
    c("Dublin", 53.35, -6.26, Capital, REGION),
    c("Athens", 37.98, 23.73, Capital, REGION),
    c("Barcelona", 41.39, 2.17, Major, LOCAL),
    c("Munich", 48.14, 11.58, Major, LOCAL),
    c("Milan", 45.46, 9.19, Major, LOCAL),
    c("Hamburg", 53.55, 9.99, Major, LOCAL),
    c("Naples", 40.85, 14.27, Major, LOCAL),
    c("St Petersburg", 59.94, 30.31, Major, LOCAL),
    // --- Middle East & Caucasus ---
    c("Ankara", 39.93, 32.86, Capital, REGION),
    c("Tehran", 35.69, 51.39, Capital, REGION),
    c("Baghdad", 33.31, 44.36, Capital, REGION),
    c("Riyadh", 24.71, 46.68, Capital, REGION),
    c("Dubai", 25.20, 55.27, Major, REGION),
    c("Doha", 25.29, 51.53, Capital, LOCAL),
    c("Kuwait City", 29.38, 47.99, Capital, LOCAL),
    c("Abu Dhabi", 24.45, 54.38, Capital, LOCAL),
    c("Muscat", 23.59, 58.41, Capital, LOCAL),
    c("Sanaa", 15.37, 44.19, Capital, LOCAL),
    c("Amman", 31.95, 35.93, Capital, LOCAL),
    c("Beirut", 33.89, 35.50, Capital, LOCAL),
    c("Damascus", 33.51, 36.29, Capital, LOCAL),
    c("Tel Aviv", 32.07, 34.78, Major, LOCAL),
    c("Baku", 40.41, 49.87, Capital, LOCAL),
    c("Tbilisi", 41.72, 44.79, Capital, LOCAL),
    c("Yerevan", 40.18, 44.51, Capital, LOCAL),
    // --- Central & South Asia ---
    c("Kabul", 34.53, 69.17, Capital, REGION),
    c("Islamabad", 33.69, 73.06, Capital, REGION),
    c("Karachi", 24.86, 67.01, Major, LOCAL),
    c("Lahore", 31.55, 74.34, Major, LOCAL),
    c("Tashkent", 41.30, 69.24, Capital, REGION),
    c("Astana", 51.17, 71.43, Capital, REGION),
    c("Ashgabat", 37.95, 58.38, Capital, LOCAL),
    c("Dhaka", 23.81, 90.41, Capital, REGION),
    c("Kathmandu", 27.72, 85.32, Capital, LOCAL),
    c("Colombo", 6.93, 79.85, Capital, LOCAL),
    c("Bangalore", 12.97, 77.59, Major, LOCAL),
    c("Kolkata", 22.57, 88.36, Major, LOCAL),
    c("Chennai", 13.08, 80.27, Major, LOCAL),
    c("Hyderabad", 17.39, 78.49, Major, LOCAL),
    // --- East & Southeast Asia ---
    c("Bangkok", 13.76, 100.50, Capital, REGION),
    c("Hanoi", 21.03, 105.85, Capital, REGION),
    c("Manila", 14.60, 120.98, Capital, REGION),
    c("Kuala Lumpur", 3.139, 101.69, Capital, REGION),
    c("Pyongyang", 39.02, 125.75, Capital, LOCAL),
    c("Taipei", 25.03, 121.57, Capital, REGION),
    c("Ulaanbaatar", 47.89, 106.91, Capital, LOCAL),
    c("Phnom Penh", 11.56, 104.92, Capital, LOCAL),
    c("Vientiane", 17.97, 102.63, Capital, LOCAL),
    c("Naypyidaw", 19.76, 96.08, Capital, LOCAL),
    c("Ho Chi Minh City", 10.82, 106.63, Major, LOCAL),
    c("Hong Kong", 22.32, 114.17, Major, LOCAL),
    c("Guangzhou", 23.13, 113.26, Major, LOCAL),
    c("Shenzhen", 22.54, 114.06, Major, LOCAL),
    c("Chengdu", 30.57, 104.07, Major, LOCAL),
    c("Chongqing", 29.56, 106.55, Major, LOCAL),
    c("Wuhan", 30.59, 114.31, Major, LOCAL),
    c("Xi'an", 34.34, 108.94, Major, LOCAL),
    c("Osaka", 34.69, 135.50, Major, LOCAL),
    c("Busan", 35.18, 129.08, Major, LOCAL),
    // --- Africa ---
    c("Algiers", 36.75, 3.06, Capital, REGION),
    c("Rabat", 34.02, -6.83, Capital, LOCAL),
    c("Casablanca", 33.57, -7.59, Major, LOCAL),
    c("Tunis", 36.81, 10.18, Capital, LOCAL),
    c("Tripoli", 32.89, 13.19, Capital, LOCAL),
    c("Alexandria", 31.20, 29.92, Major, LOCAL),
    c("Khartoum", 15.50, 32.56, Capital, REGION),
    c("Addis Ababa", 9.03, 38.74, Capital, REGION),
    c("Nairobi", -1.29, 36.82, Capital, REGION),
    c("Kampala", 0.35, 32.58, Capital, LOCAL),
    c("Dar es Salaam", -6.79, 39.21, Major, LOCAL),
    c("Kinshasa", -4.32, 15.31, Capital, REGION),
    c("Luanda", -8.84, 13.23, Capital, LOCAL),
    c("Accra", 5.60, -0.19, Capital, REGION),
    c("Abuja", 9.06, 7.50, Capital, LOCAL),
    c("Dakar", 14.72, -17.47, Capital, REGION),
    c("Abidjan", 5.36, -4.01, Major, LOCAL),
    c("Bamako", 12.64, -8.00, Capital, LOCAL),
    c("Harare", -17.83, 31.05, Capital, LOCAL),
    c("Lusaka", -15.42, 28.28, Capital, LOCAL),
    c("Maputo", -25.97, 32.57, Capital, LOCAL),
    c("Pretoria", -25.75, 28.19, Capital, REGION),
    c("Johannesburg", -26.20, 28.05, Major, LOCAL),
    c("Cape Town", -33.92, 18.42, Major, LOCAL),
    c("Mogadishu", 2.05, 45.34, Capital, LOCAL),
    c("Antananarivo", -18.88, 47.51, Capital, LOCAL),
    // --- North & Central America & Caribbean ---
    c("Washington", 38.91, -77.04, Capital, REGION),
    c("Ottawa", 45.42, -75.70, Capital, REGION),
    c("Toronto", 43.65, -79.38, Major, LOCAL),
    c("Montreal", 45.50, -73.57, Major, LOCAL),
    c("Vancouver", 49.28, -123.12, Major, LOCAL),
    c("Chicago", 41.88, -87.63, Major, LOCAL),
    c("San Francisco", 37.77, -122.42, Major, LOCAL),
    c("Houston", 29.76, -95.37, Major, LOCAL),
    c("Miami", 25.76, -80.19, Major, LOCAL),
    c("Seattle", 47.61, -122.33, Major, LOCAL),
    c("Boston", 42.36, -71.06, Major, LOCAL),
    c("Atlanta", 33.75, -84.39, Major, LOCAL),
    c("Havana", 23.11, -82.37, Capital, REGION),
    c("Santo Domingo", 18.49, -69.93, Capital, LOCAL),
    c("Kingston", 18.02, -76.80, Capital, LOCAL),
    c("Guatemala City", 14.63, -90.51, Capital, LOCAL),
    c("Panama City", 8.98, -79.52, Capital, LOCAL),
    c("San Jose", 9.93, -84.09, Capital, LOCAL),
    c("Guadalajara", 20.67, -103.35, Major, LOCAL),
    c("Monterrey", 25.69, -100.32, Major, LOCAL),
    // --- South America ---
    c("Bogota", 4.71, -74.07, Capital, REGION),
    c("Lima", -12.05, -77.04, Capital, REGION),
    c("Santiago", -33.45, -70.67, Capital, REGION),
    c("Buenos Aires", -34.61, -58.38, Capital, GLOBAL),
    c("Caracas", 10.49, -66.88, Capital, REGION),
    c("Quito", -0.18, -78.47, Capital, LOCAL),
    c("La Paz", -16.50, -68.15, Capital, LOCAL),
    c("Asuncion", -25.30, -57.64, Capital, LOCAL),
    c("Montevideo", -34.90, -56.16, Capital, LOCAL),
    c("Brasilia", -15.79, -47.88, Capital, REGION),
    c("Rio de Janeiro", -22.91, -43.17, Major, LOCAL),
    c("Medellin", 6.24, -75.57, Major, LOCAL),
    // --- Oceania ---
    c("Canberra", -35.28, 149.13, Capital, REGION),
    c("Melbourne", -37.81, 144.96, Major, LOCAL),
    c("Perth", -31.95, 115.86, Major, LOCAL),
    c("Brisbane", -27.47, 153.03, Major, LOCAL),
    c("Wellington", -41.29, 174.78, Capital, REGION),
    c("Auckland", -36.85, 174.76, Major, LOCAL),
    c("Port Moresby", -9.44, 147.18, Capital, LOCAL),
    c("Suva", -18.14, 178.44, Capital, LOCAL),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gazetteer_is_non_empty_and_well_formed() {
        assert!(CITIES.len() > 100, "a worldwide set is embedded");
        for city in CITIES {
            assert!(!city.name.is_empty(), "every city is named");
            assert!(
                (-90.0..=90.0).contains(&city.lat),
                "{} latitude in range",
                city.name
            );
            assert!(
                (-180.0..=180.0).contains(&city.lon),
                "{} longitude in range",
                city.name
            );
            assert!(
                city.label_span > 0.0 && city.label_span <= 360.0,
                "{} label span sane",
                city.name
            );
        }
    }

    #[test]
    fn every_continent_is_represented() {
        // A coarse spot-check that the table reaches each major landmass, so a
        // future edit can't silently gut a region.
        let has = |name: &str| CITIES.iter().any(|c| c.name == name);
        for anchor in [
            "London",
            "Tokyo",
            "Cairo",
            "New York",
            "Sao Paulo",
            "Sydney",
        ] {
            assert!(has(anchor), "{anchor} present");
        }
    }

    #[test]
    fn labels_reveal_progressively_as_you_zoom_in() {
        // Number of cities whose name would draw at a given viewport span.
        let visible_at = |span: f64| CITIES.iter().filter(|c| span <= c.label_span).count();

        // The whole-globe default (360°) shows dots only — no names overprint.
        assert_eq!(visible_at(360.0), 0, "globe view is dots-only");
        // The first zoom-in brings a handful of megacity anchors.
        let anchors = visible_at(180.0);
        assert!(
            (1..=30).contains(&anchors),
            "first zoom reveals anchors (got {anchors})"
        );
        // A regional zoom adds the capitals.
        assert!(visible_at(90.0) > anchors, "regional zoom adds capitals");
        // A close zoom shows essentially the whole table.
        assert!(
            visible_at(20.0) > CITIES.len() * 3 / 4,
            "close zoom shows most cities"
        );
    }
}
