// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The areas an instance can be in: a country, and optionally one of its top
//! level subdivisions.
//!
//! The list is closed. An instance's area is chosen from it, stored as ISO
//! codes and shown by looking the names up here, so nothing anyone types ever
//! reaches the page as a name. It is `assets/areas.json`, built from Debian's
//! iso-codes by `scripts/update-areas.py`, and compiled in.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::LazyLock;

/// A language the names are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Spanish,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Spanish => "es",
        }
    }
}

#[derive(Debug, Deserialize)]
struct Document {
    source: String,
    countries: Vec<Country>,
}

/// A country (ISO 3166-1) and its top level subdivisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Country {
    /// Its alpha-2 code.
    pub code: String,
    /// Its name, by language code.
    pub names: HashMap<String, String>,
    pub regions: Vec<Region>,
}

/// A top level subdivision (ISO 3166-2) of a country.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    /// Its code, as `ES-GA`.
    pub code: String,
    /// Its ISO name, which is the local one, in every language.
    pub name: String,
}

impl Country {
    pub fn name(&self, language: Language) -> &str {
        self.names
            .get(language.code())
            .or_else(|| self.names.get("en"))
            .map(String::as_str)
            .unwrap_or(&self.code)
    }

    pub fn region(&self, code: &str) -> Option<&Region> {
        self.regions.iter().find(|region| region.code == code)
    }
}

/// The whole list.
pub struct Areas {
    source: String,
    countries: Vec<Country>,
    index: HashMap<String, usize>,
}

static AREAS: LazyLock<Areas> = LazyLock::new(|| {
    let document: Document = serde_json::from_str(include_str!("../assets/areas.json"))
        .expect("assets/areas.json is valid");
    let index = document
        .countries
        .iter()
        .enumerate()
        .map(|(i, country)| (country.code.clone(), i))
        .collect();
    Areas {
        source: document.source,
        countries: document.countries,
        index,
    }
});

/// Why an area was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AreaError {
    UnknownCountry(String),
    UnknownRegion { country: String, region: String },
}

impl fmt::Display for AreaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCountry(code) => write!(f, "{code:?} is not a known country"),
            Self::UnknownRegion { country, region } => {
                write!(f, "{region:?} is not a region of {country}")
            }
        }
    }
}

impl std::error::Error for AreaError {}

impl Areas {
    pub fn get() -> &'static Self {
        &AREAS
    }

    /// Where the list came from, as "iso-codes 4.20.1".
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn countries(&self) -> &[Country] {
        &self.countries
    }

    pub fn country(&self, code: &str) -> Option<&Country> {
        self.index.get(code).map(|&i| &self.countries[i])
    }

    /// Checks an area, and answers it in its canonical form: codes in upper
    /// case, and an empty region as none.
    pub fn check(
        &self,
        country: &str,
        region: Option<&str>,
    ) -> Result<(String, Option<String>), AreaError> {
        let country_code = country.trim().to_ascii_uppercase();
        let Some(found) = self.country(&country_code) else {
            return Err(AreaError::UnknownCountry(country.to_string()));
        };
        let region = region.map(str::trim).filter(|region| !region.is_empty());
        match region {
            None => Ok((country_code, None)),
            Some(region) => {
                let region_code = region.to_ascii_uppercase();
                match found.region(&region_code) {
                    Some(_) => Ok((country_code, Some(region_code))),
                    None => Err(AreaError::UnknownRegion {
                        country: country_code,
                        region: region.to_string(),
                    }),
                }
            }
        }
    }

    /// The name of an area, as "Galicia, Spain", or "Spain" without a region.
    /// Codes not in the list, which a database written by a build with a
    /// newer list could hold, are shown as they are.
    pub fn label(&self, country: &str, region: Option<&str>, language: Language) -> String {
        let found = self.country(country);
        let country_name = found.map_or(country, |found| found.name(language));
        match region {
            None => country_name.to_string(),
            Some(region) => {
                let region_name = found
                    .and_then(|found| found.region(region))
                    .map_or(region, |found| found.name.as_str());
                format!("{region_name}, {country_name}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_complete_and_well_formed() {
        let areas = Areas::get();
        assert!(areas.source().starts_with("iso-codes "));
        assert!(areas.countries().len() > 240);
        for country in areas.countries() {
            assert_eq!(country.code.len(), 2, "{}", country.code);
            assert!(country.names.contains_key("en"), "{}", country.code);
            assert!(country.names.contains_key("es"), "{}", country.code);
            for region in &country.regions {
                assert!(
                    region.code.starts_with(&format!("{}-", country.code)),
                    "{}",
                    region.code
                );
                assert!(!region.name.is_empty() && !region.name.contains('['));
            }
        }
    }

    #[test]
    fn areas_are_checked_and_made_canonical() {
        let areas = Areas::get();
        assert_eq!(
            areas.check("es", Some("es-ga")).unwrap(),
            ("ES".to_string(), Some("ES-GA".to_string()))
        );
        assert_eq!(
            areas.check("ES", Some(" ")).unwrap(),
            ("ES".to_string(), None)
        );
        assert_eq!(
            areas.check("XX", None),
            Err(AreaError::UnknownCountry("XX".into()))
        );
        // A region of another country is not one of this one.
        assert!(matches!(
            areas.check("PT", Some("ES-GA")),
            Err(AreaError::UnknownRegion { .. })
        ));
        // Nor is free text.
        assert!(areas.check("ES", Some("Somewhere nice")).is_err());
    }

    #[test]
    fn labels_are_in_the_language_asked_for() {
        let areas = Areas::get();
        assert_eq!(
            areas.label("ES", Some("ES-GA"), Language::English),
            "Galicia, Spain"
        );
        assert_eq!(
            areas.label("ES", Some("ES-GA"), Language::Spanish),
            "Galicia, España"
        );
        assert_eq!(areas.label("US", None, Language::Spanish), "Estados Unidos");
        assert_eq!(
            areas.label("ZZ", Some("ZZ-Q"), Language::English),
            "ZZ-Q, ZZ"
        );
    }
}
