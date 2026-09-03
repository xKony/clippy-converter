use crate::db::Db;
use crate::models::{Config, ConversionResult, ConvertedValue, UnitEntry, UnitInfo};
use crate::parser::{ParsedInput, ResolvedZone, WallClock, resolve_zone};
use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, Local, Offset, TimeZone, Timelike, Utc};

/// A unit with precomputed lowercase forms so search never allocates per query.
#[derive(Debug, Clone)]
pub struct SearchableUnit {
    /// The unit's display data (symbol and aliases).
    pub info: UnitInfo,
    /// Lowercased symbol.
    symbol_lower: String,
    /// Lowercased aliases.
    aliases_lower: Vec<String>,
}

impl SearchableUnit {
    fn new(info: UnitInfo) -> Self {
        let symbol_lower = info.symbol.to_lowercase();
        let aliases_lower = info.aliases.iter().map(|a| a.to_lowercase()).collect();
        Self {
            info,
            symbol_lower,
            aliases_lower,
        }
    }

    /// Returns `true` if the lowercased query is a substring of the symbol or any alias.
    #[must_use]
    pub fn matches(&self, query_lower: &str) -> bool {
        self.symbol_lower.contains(query_lower)
            || self.aliases_lower.iter().any(|a| a.contains(query_lower))
    }

    /// Returns `true` if the lowercased query equals the symbol or any alias exactly.
    #[must_use]
    pub fn matches_exact(&self, query_lower: &str) -> bool {
        self.symbol_lower == query_lower || self.aliases_lower.iter().any(|a| a == query_lower)
    }
}

/// The core conversion engine.
pub struct Converter {
    /// User configuration for sorting and limits.
    config: Config,
    /// Database handle for currency rates and units.
    db: Db,
    /// Cached unit list to avoid repeated full-table reads during UI frames.
    units_cache: Option<Vec<SearchableUnit>>,
}

impl Converter {
    /// Creates a new `Converter` with the provided configuration and database handle.
    #[must_use]
    pub const fn new(config: Config, db: Db) -> Self {
        Self {
            config,
            db,
            units_cache: None,
        }
    }

    /// Replaces the configuration (e.g. after a favorites change) without
    /// discarding the cached unit list.
    pub fn set_config(&mut self, config: Config) {
        if self.config.unit_packs != config.unit_packs {
            self.units_cache = None;
        }
        self.config = config;
    }

    /// Clears the cached unit list so the next [`Self::all_units`] call reloads from the DB.
    ///
    /// Call this after background rate refreshes add or update symbols.
    pub fn invalidate_units_cache(&mut self) {
        self.units_cache = None;
    }

    /// Returns a borrowed slice of all supported units with their aliases and
    /// precomputed lowercase search forms, populating the internal cache on
    /// first use to avoid repeated full-table reads and per-call allocations.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn all_units(&mut self) -> Result<&[SearchableUnit]> {
        if self.units_cache.is_none() {
            let unit_map = self.db.get_all_units_with_aliases()?;
            let mut result: Vec<SearchableUnit> = unit_map
                .into_iter()
                .filter(|(_, (_, category))| self.config.unit_packs.allows(*category))
                .map(|(symbol, (aliases, category))| {
                    SearchableUnit::new(UnitInfo {
                        symbol,
                        aliases,
                        category,
                    })
                })
                .collect();
            result.sort_by(|a, b| a.info.symbol.cmp(&b.info.symbol));
            self.units_cache = Some(result);
        }
        // Cache is populated above whenever it was `None`.
        Ok(self.units_cache.as_deref().unwrap_or(&[]))
    }

    /// Returns `true` when `symbol` (or an alias) resolves to a known
    /// currency unit.
    ///
    /// Database read failures count as "unknown": callers only gate optional
    /// behavior (default-target pinning) on this, and the real conversion
    /// path surfaces any genuine database error itself.
    #[must_use]
    pub fn is_currency_unit(&self, symbol: &str) -> bool {
        let Ok(resolved) = self.db.resolve_symbol(symbol) else {
            return false;
        };
        self.db
            .get_unit(&resolved)
            .ok()
            .flatten()
            .is_some_and(|entry| entry.category == crate::models::UnitCategory::Currency as u8)
    }

    /// Converts a numeric value from one unit to all compatible target units.
    ///
    /// # Errors
    /// Returns an error if the input unit is unknown or if the conversion fails.
    pub fn convert(&self, value: f64, from_input: &str) -> Result<ConversionResult> {
        self.convert_preferring(value, from_input, None)
    }

    /// Resolves one side of a compound unit to its current entry, accepting
    /// metric-prefixed (`kilo`) and counted (`100km`) fragments. Returns `None`
    /// when the fragment does not name a usable unit; currency rows are never
    /// reached through prefix/count fallbacks, mirroring the single-unit path.
    fn lookup_component(&self, fragment: &str) -> Result<Option<UnitEntry>> {
        if let Ok(resolved) = self.db.resolve_symbol(fragment)
            && let Ok(Some(entry)) = self.db.get_unit(&resolved)
        {
            return Ok(Some(entry));
        }
        let scaled = extract_metric_prefix(fragment).or_else(|| leading_count_factor(fragment));
        if let Some((scale, rest)) = scaled
            && let Ok(resolved) = self.db.resolve_symbol(rest)
            && let Ok(Some(mut entry)) = self.db.get_unit(&resolved)
            && entry.category != crate::models::UnitCategory::Currency as u8
        {
            entry.factor *= scale;
            return Ok(Some(entry));
        }
        Ok(None)
    }

    /// Computes the live factor of a compound unit from its components:
    /// `factor(numerator) / factor(denominator)`. Returns `None` when either
    /// side is missing or unusable in a linear ratio (affine units such as
    /// temperature, zero/non-finite factors).
    fn resolve_compound_factor(&self, numerator: &str, denominator: &str) -> Result<Option<f64>> {
        let Some(num) = self.lookup_component(numerator)? else {
            return Ok(None);
        };
        let Some(den) = self.lookup_component(denominator)? else {
            return Ok(None);
        };
        if num.offset != 0.0 || den.offset != 0.0 {
            // Affine units break the linear-ratio model.
            return Ok(None);
        }
        if num.factor == 0.0
            || den.factor == 0.0
            || !num.factor.is_finite()
            || !den.factor.is_finite()
        {
            return Ok(None);
        }
        Ok(Some(num.factor / den.factor))
    }

    /// Category of a compound component, used to keep output families coherent
    /// so price-per-mass converts to price-per-mass and wages to wages.
    fn component_category(&self, fragment: &str) -> Option<u8> {
        self.lookup_component(fragment)
            .ok()
            .flatten()
            .map(|entry| entry.category)
    }

    /// Fills `outputs` with sibling compound units: seeded `Compound` rows
    /// whose numerator and denominator categories both match the source's.
    /// Target factors are recomposed live exactly like the source's, ignoring
    /// the placeholder factors stored on the seeded rows.
    fn push_compound_outputs(
        &self,
        outputs: &mut Vec<ConvertedValue>,
        base_value: f64,
        from_unit: &str,
        numerator: &str,
        denominator: &str,
    ) -> Result<()> {
        let Some(num_cat) = self.component_category(numerator) else {
            return Ok(());
        };
        let Some(den_cat) = self.component_category(denominator) else {
            return Ok(());
        };
        let siblings = self
            .db
            .get_category_units(crate::models::UnitCategory::Compound as u8)?;
        for (symbol, _placeholder) in siblings {
            if symbol == from_unit {
                continue;
            }
            let Some((target_num, target_den)) = decompose_compound(&symbol) else {
                continue;
            };
            if self.component_category(target_num) != Some(num_cat)
                || self.component_category(target_den) != Some(den_cat)
            {
                continue;
            }
            if let Some(target_factor) = self.resolve_compound_factor(target_num, target_den)? {
                outputs.push(ConvertedValue {
                    value: base_value / target_factor,
                    unit: symbol,
                });
            }
        }
        Ok(())
    }

    /// Like [`Self::convert`], but pins `prefer` at the front of the output list
    /// when that symbol exists in the same category.
    ///
    /// # Errors
    /// Returns an error if the input unit is unknown or if the conversion fails.
    pub fn convert_preferring(
        &self,
        value: f64,
        from_input: &str,
        prefer: Option<&str>,
    ) -> Result<ConversionResult> {
        let mut actual_value = value;
        let mut parsed_unit = from_input;

        // 1. Check for currency multipliers (e.g., "B USD")
        if let Some((factor, rest)) = extract_currency_multiplier(from_input) {
            let resolved_rest = self.db.resolve_symbol(rest)?;
            if let Ok(Some(entry)) = self.db.get_unit(&resolved_rest)
                && entry.category == crate::models::UnitCategory::Currency as u8
            {
                actual_value *= factor;
                parsed_unit = rest;
            }
        }

        // Resolve "kilometers" to "km"
        let mut from_unit = self.db.resolve_symbol(parsed_unit)?;

        let mut entry_opt = self.db.get_unit(&from_unit)?;

        // 2. Metric prefix fallback
        if entry_opt.is_none()
            && let Some((factor, rest)) = extract_metric_prefix(parsed_unit)
        {
            let resolved_rest = self.db.resolve_symbol(rest)?;
            if let Ok(Some(rest_entry)) = self.db.get_unit(&resolved_rest)
                && rest_entry.category != crate::models::UnitCategory::Currency as u8
            {
                actual_value *= factor;
                from_unit = resolved_rest;
                entry_opt = Some(rest_entry);
            }
        }

        // 3. Compound/rate units (`USD/kg`, `kWh/100km`). Seeded rows exist for
        // discovery but carry placeholder factors only, so whenever a
        // Compound-category row (or an unseeded `/` fragment) shows up, the
        // factor is recomposed live from the current component rows - keeping
        // currency-based rates fresh across API refreshes.
        let is_compound_row = entry_opt
            .as_ref()
            .is_some_and(|e| e.category == crate::models::UnitCategory::Compound as u8);
        let mut compound_parts = None;
        if (is_compound_row || entry_opt.is_none())
            && let Some((numerator, denominator)) = decompose_compound(parsed_unit)
        {
            match self.resolve_compound_factor(numerator, denominator)? {
                Some(factor) => {
                    entry_opt = Some(UnitEntry {
                        factor,
                        offset: 0.0,
                        category: crate::models::UnitCategory::Compound as u8,
                        timestamp: 0,
                        source: crate::models::RateSource::Static as u8,
                    });
                    compound_parts = Some((numerator, denominator));
                }
                // A seeded compound whose components fail to resolve (no
                // rates cached yet, temperature denominator) must not fall
                // back to its placeholder factor.
                None if is_compound_row => {
                    return Err(anyhow!("Unknown unit: {from_input}"));
                }
                None => {}
            }
        }
        let entry = entry_opt.ok_or_else(|| anyhow!("Unknown unit: {from_input}"))?;

        // Math: Base = (Input + Offset) * Factor
        let base_value = (actual_value + entry.offset) * entry.factor;

        let mut outputs = Vec::new();
        if let Some((numerator, denominator)) = compound_parts {
            self.push_compound_outputs(
                &mut outputs,
                base_value,
                &from_unit,
                numerator,
                denominator,
            )?;
        } else {
            let targets = self.db.get_category_units(entry.category)?;

            for (symbol, target_entry) in targets {
                if symbol != from_unit {
                    // Math: Target = (Base / Factor) - Offset
                    let target_val = (base_value / target_entry.factor) - target_entry.offset;
                    outputs.push(ConvertedValue {
                        value: target_val,
                        unit: symbol,
                    });
                }
            }
        }

        // Deduplicate units
        outputs.sort_by(|a, b| a.unit.cmp(&b.unit));
        outputs.dedup_by(|a, b| a.unit == b.unit);

        let ranks = crate::models::favorite_ranks(&self.config.favorites);
        outputs.sort_by(|a, b| crate::models::cmp_favorite_rank(&a.unit, &b.unit, &ranks));

        if let Some(prefer) = prefer.filter(|s| !s.is_empty()) {
            let resolved = self.db.resolve_symbol(prefer)?;
            if let Some(idx) = outputs.iter().position(|o| o.unit == resolved) {
                let preferred = outputs.remove(idx);
                outputs.insert(0, preferred);
            }
        }

        Ok(ConversionResult {
            input_value: value,
            input_unit: from_input.to_string(),
            outputs,
        })
    }

    /// Converts both endpoints of a parsed range such as `10-20 USD`.
    ///
    /// Each endpoint runs through the same conversion path as
    /// [`Self::convert_preferring`]; the result keeps one row per endpoint in
    /// start-then-end order, holding that endpoint's top-ranked conversion.
    ///
    /// # Errors
    /// Returns an error if the input unit is unknown or if the conversion fails.
    pub fn convert_range_preferring(
        &self,
        start: f64,
        end: f64,
        from_input: &str,
        prefer: Option<&str>,
    ) -> Result<ConversionResult> {
        let mut outputs: Vec<ConvertedValue> = Vec::new();
        let mut input_unit = String::new();
        for value in [start, end] {
            let result = self.convert_preferring(value, from_input, prefer)?;
            if input_unit.is_empty() {
                input_unit = result.input_unit;
            }
            if let Some(row) = result.outputs.first() {
                // Collapses a degenerate range (`10-10 USD`) to one row instead
                // of two identical ones; `to_bits` compares floats exactly.
                let duplicate = outputs.last().is_some_and(|previous| {
                    previous.unit == row.unit && previous.value.to_bits() == row.value.to_bits()
                });
                if !duplicate {
                    outputs.push(row.clone());
                }
            }
        }

        Ok(ConversionResult {
            input_value: start,
            input_unit,
            outputs,
        })
    }

    /// Converts a parser result, emitting one row per endpoint when a range
    /// (`10-20 USD`, `10 to 20 km`) was detected, behaving like the timezone
    /// branch when a wall-clock payload is present, and otherwise exactly
    /// like [`Self::convert_preferring`].
    ///
    /// # Errors
    /// Returns an error when no source unit is present or if the conversion fails.
    pub fn convert_parsed(&self, parsed: &ParsedInput) -> Result<ConversionResult> {
        if let Some(moment) = &parsed.wall_clock {
            // Wall-clock inputs always carry their raw text in `unit` so the
            // popup header can echo them; fall back defensively regardless.
            let input_text = parsed.unit.as_deref().unwrap_or("datetime");
            return Self::convert_wall_clock(moment, input_text, parsed.target.as_deref());
        }
        let Some(unit) = parsed.unit.as_deref() else {
            return Err(anyhow!("parsed input carries no source unit"));
        };
        parsed.end_value.map_or_else(
            || self.convert_preferring(parsed.value, unit, parsed.target.as_deref()),
            |end| self.convert_range_preferring(parsed.value, end, unit, parsed.target.as_deref()),
        )
    }

    /// Renders a wall-clock moment as formatted datetime rows.
    ///
    /// Every row's `value` stays the Unix epoch so Enter/copy yields a number
    /// (the epoch for datetime sources), while the `unit` label carries the
    /// human-readable rendering like `2026-08-21 19:30 (local, CEST)`. Rows
    /// are ordered explicit-target first (like [`Self::convert_preferring`]
    /// pins currencies), then the OS-local zone, then a UTC reference.
    fn convert_wall_clock(
        moment: &WallClock,
        input_text: &str,
        prefer: Option<&str>,
    ) -> Result<ConversionResult> {
        let epoch = moment.epoch_seconds;
        let utc_moment = Utc
            .timestamp_opt(epoch, 0)
            .single()
            .ok_or_else(|| anyhow!("Time out of representable range: {epoch}"))?;

        #[expect(
            clippy::cast_precision_loss,
            reason = "epoch seconds stay far below f64's exact-integer range"
        )]
        let epoch_value = epoch as f64;

        let mut outputs: Vec<ConvertedValue> = Vec::new();
        if let Some(target) = prefer.filter(|zone| !zone.is_empty()) {
            match resolve_zone(target) {
                Some(ResolvedZone::Utc) => {
                    outputs.push(utc_reference_row(epoch_value, &utc_moment));
                }
                Some(ResolvedZone::Fixed(offset)) => {
                    let rendered = utc_moment.with_timezone(&offset);
                    // The suffix is the offset itself; `%Z` would just echo it.
                    outputs.push(datetime_row(
                        epoch_value,
                        &moment_label(&rendered),
                        &offset_label(offset),
                    ));
                }
                Some(ResolvedZone::Tz(zone)) => {
                    let rendered = utc_moment.with_timezone(&zone);
                    outputs.push(datetime_row(
                        epoch_value,
                        &moment_label(&rendered),
                        &zone_suffix(&rendered, zone.name()),
                    ));
                }
                // `resolve_zone` never produces `Local`; anything unresolved
                // is a user-facing unknown-zone error.
                Some(ResolvedZone::Local) | None => {
                    return Err(anyhow!("Unknown timezone: {target}"));
                }
            }
        }

        let rendered_local = utc_moment.with_timezone(&Local);
        outputs.push(datetime_row(
            epoch_value,
            &moment_label(&rendered_local),
            &zone_suffix(&rendered_local, "local"),
        ));
        outputs.push(utc_reference_row(epoch_value, &utc_moment));

        // Collapse identical renderings regardless of position (an explicit
        // UTC target sits rows away from the trailing reference row); the
        // first occurrence wins so pinned targets keep their spot.
        let mut collapsed: Vec<ConvertedValue> = Vec::with_capacity(outputs.len());
        for row in outputs {
            if !collapsed.iter().any(|kept| kept.unit == row.unit) {
                collapsed.push(row);
            }
        }

        Ok(ConversionResult {
            input_value: epoch_value,
            input_unit: input_text.to_string(),
            outputs: collapsed,
        })
    }
}

/// Builds one datetime row from its preformatted wall clock and zone suffix.
fn datetime_row(value: f64, moment: &str, suffix: &str) -> ConvertedValue {
    ConvertedValue {
        value,
        unit: format!("{moment} ({suffix})"),
    }
}

/// The UTC reference row shared by pinned targets and the trailing default.
fn utc_reference_row(epoch: f64, utc_moment: &DateTime<Utc>) -> ConvertedValue {
    ConvertedValue {
        value: epoch,
        unit: format!("{} (UTC)", moment_label(utc_moment)),
    }
}

/// Formats the wall clock of a rendered datetime, seconds only when nonzero.
fn moment_label<Z>(rendered: &DateTime<Z>) -> String
where
    Z: TimeZone,
    Z::Offset: std::fmt::Display,
{
    let base = rendered.format("%Y-%m-%d %H:%M").to_string();
    if rendered.second() == 0 {
        base
    } else {
        format!("{base}:{}", rendered.format("%S"))
    }
}

/// Composes a row suffix such as `local, CEST` or `Europe/Warsaw, CET`.
///
/// `%Z` abbreviations are platform-opaque for `Local` (often empty or an
/// offset on Windows), so anything empty or offset-shaped falls back to the
/// numeric offset which is always available from chrono itself.
fn zone_suffix<Z>(rendered: &DateTime<Z>, zone_label: &str) -> String
where
    Z: TimeZone,
    Z::Offset: std::fmt::Display,
{
    let abbreviation = rendered.format("%Z").to_string();
    let tag = if abbreviation.is_empty() || abbreviation.starts_with(['+', '-']) {
        offset_label(rendered.offset().fix())
    } else {
        abbreviation
    };
    format!("{zone_label}, {tag}")
}

/// Formats a fixed offset as `UTC+05:30` / `UTC-08:00`.
fn offset_label(offset: FixedOffset) -> String {
    let seconds = offset.local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let absolute = seconds.abs();
    format!(
        "UTC{sign}{:02}:{:02}",
        absolute / 3600,
        absolute % 3600 / 60
    )
}

/// Matches an ASCII prefix table against `input`.
///
/// # Safety invariant
/// All prefixes must be pure ASCII so that `prefix.len()` is valid
/// for slicing both the lowercase and original mixed-case strings.
fn prefix_factor<'a>(input: &'a str, prefixes: &[(&str, f64)]) -> Option<(f64, &'a str)> {
    let lower = input.to_lowercase();
    for &(prefix, factor) in prefixes {
        debug_assert!(prefix.is_ascii(), "prefixes must be ASCII");
        if lower.starts_with(prefix) {
            return Some((factor, &input[prefix.len()..]));
        }
    }
    None
}

/// Extracts a currency multiplier from the start of the unit string.
fn extract_currency_multiplier(input: &str) -> Option<(f64, &str)> {
    let input = input.trim();
    prefix_factor(
        input,
        &[
            ("k ", 1e3),
            ("m ", 1e6),
            ("b ", 1e9),
            ("t ", 1e12),
            ("thousand ", 1e3),
            ("million ", 1e6),
            ("billion ", 1e9),
            ("trillion ", 1e12),
        ],
    )
    .map(|(factor, rest)| (factor, rest.trim()))
}

/// Extracts a metric prefix (e.g., "kilo", "nano") from the start of the unit string.
fn extract_metric_prefix(input: &str) -> Option<(f64, &str)> {
    prefix_factor(
        input,
        &[
            ("exa", 1e18),
            ("peta", 1e15),
            ("tera", 1e12),
            ("giga", 1e9),
            ("mega", 1e6),
            ("kilo", 1e3),
            ("hecto", 1e2),
            ("deca", 1e1),
            ("deci", 1e-1),
            ("centi", 1e-2),
            ("milli", 1e-3),
            ("micro", 1e-6),
            ("nano", 1e-9),
            ("pico", 1e-12),
            ("femto", 1e-15),
            ("atto", 1e-18),
        ],
    )
}

/// Splits a compound/rate unit such as `USD/kg` into its numerator and
/// denominator fragments. Rejects anything without exactly one `/` separator
/// bounded by non-empty sides.
fn decompose_compound(unit: &str) -> Option<(&str, &str)> {
    let (numerator, denominator) = unit.split_once('/')?;
    let numerator = numerator.trim();
    let denominator = denominator.trim();
    if numerator.is_empty() || denominator.is_empty() {
        return None;
    }
    Some((numerator, denominator))
}

/// Splits a leading integer count off a fragment such as the `100` in
/// `kWh/100km`, returning `(count, rest)`.
fn leading_count_factor(fragment: &str) -> Option<(f64, &str)> {
    let digits_end = fragment.find(|c: char| !c.is_ascii_digit())?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "unit scale counts stay far below f64's exact-integer range"
    )]
    let count = fragment[..digits_end].parse::<u64>().ok()? as f64;
    Some((count, &fragment[digits_end..]))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::float_cmp,
        // Test epochs are small enough to be exact in f64.
        clippy::cast_precision_loss
    )]
    use super::*;
    use crate::models::{RateSource, UnitCategory};
    use redb::Database;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    /// Relative-tolerance float assertion: tolerance scales with magnitude so
    /// large conversion results don't fail on accumulated rounding error.
    fn assert_relative_eq(a: f64, b: f64) {
        let tolerance = 1e-9 * a.abs().max(b.abs()).max(1.0);
        assert!(
            (a - b).abs() <= tolerance,
            "expected {a} to equal {b} within relative tolerance"
        );
    }

    fn create_test_db() -> Db {
        let tmp_file = NamedTempFile::new().unwrap();
        let db_inner = Database::builder().create(tmp_file.path()).unwrap();
        let db = Db::open_for_test(Arc::new(db_inner));
        db.init_static_units().unwrap();
        db
    }

    #[test]
    fn test_length_conversion() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "m").unwrap();
        let cm = res.outputs.iter().find(|o| o.unit == "cm").unwrap();
        assert_relative_eq(cm.value, 100.0);
    }

    #[test]
    fn test_currency_conversion() {
        let config = Config::default();
        let db = create_test_db();
        // EUR is base (factor 1.0, offset 0.0)
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        // USD (e.g. 1.1 USD per 1 EUR) -> factor = 1/1.1
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();

        let converter = Converter::new(config, db);

        let res = converter.convert(10.0, "EUR").unwrap();
        let usd = res.outputs.iter().find(|o| o.unit == "USD").unwrap();
        // Target = (Base / Factor) - Offset
        // Target_USD = (10.0 / (1.0/1.1)) - 0 = 11.0
        assert_relative_eq(usd.value, 11.0);
    }

    #[test]
    fn usd_per_kg_converts_to_usd_per_lb_via_denominator_factors() {
        let db = create_test_db();
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();
        let converter = Converter::new(Config::default(), db);

        let res = converter.convert(10.0, "USD/kg").unwrap();
        // The currency factor cancels: 10 USD/kg = 10 * (lb-factor / kg-factor).
        let per_lb = res.outputs.iter().find(|o| o.unit == "USD/lb").unwrap();
        assert_relative_eq(per_lb.value, 10.0 * 453.592_37 / 1000.0);
    }

    #[test]
    fn compound_wage_tracks_the_live_cross_currency_rate() {
        let db = create_test_db();
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();
        let converter = Converter::new(Config::default(), db);

        let res = converter.convert(10.0, "USD/h").unwrap();
        let per_hour_eur = res.outputs.iter().find(|o| o.unit == "EUR/h").unwrap();
        assert_relative_eq(per_hour_eur.value, 10.0 / 1.1);
    }

    #[test]
    fn energy_per_distance_handles_the_counted_denominator() {
        let converter = Converter::new(Config::default(), create_test_db());

        // 10 kWh/100km = 10 * 1609.344 / 100_000 kWh/mi
        let res = converter.convert(10.0, "kWh/100km").unwrap();
        let per_mi = res.outputs.iter().find(|o| o.unit == "kWh/mi").unwrap();
        assert_relative_eq(per_mi.value, 10.0 * 1609.344 / 100_000.0);
    }

    #[test]
    fn lowercase_compound_alias_resolves_like_the_canonical_symbol() {
        let db = create_test_db();
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();
        let converter = Converter::new(Config::default(), db);

        let res = converter.convert(10.0, "usd/kg").unwrap();
        assert!(res.outputs.iter().any(|o| o.unit == "USD/lb"));
    }

    #[test]
    fn explicit_target_pins_a_sibling_compound_unit() {
        let db = create_test_db();
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();
        let converter = Converter::new(Config::default(), db);

        let res = converter
            .convert_preferring(10.0, "USD/kg", Some("EUR/kg"))
            .unwrap();
        assert_eq!(res.outputs.first().map(|o| o.unit.as_str()), Some("EUR/kg"));
        assert_relative_eq(res.outputs[0].value, 10.0 / 1.1);
    }

    #[test]
    fn temperature_denominator_is_rejected_instead_of_misconverting() {
        let converter = Converter::new(Config::default(), create_test_db());

        let res = converter.convert(10.0, "USD/C");
        assert!(res.is_err());
    }

    #[test]
    fn unknown_component_falls_back_to_unknown_unit_error() {
        let converter = Converter::new(Config::default(), create_test_db());

        let res = converter.convert(10.0, "foo/bar");
        assert!(res.is_err());
    }

    #[test]
    fn test_temperature_conversion() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        // 0 C to F
        let res = converter.convert(0.0, "C").unwrap();
        let f = res.outputs.iter().find(|o| o.unit == "F").unwrap();
        assert_relative_eq(f.value, 32.0);

        // 32 F to C
        let res = converter.convert(32.0, "F").unwrap();
        let c = res.outputs.iter().find(|o| o.unit == "C").unwrap();
        assert_relative_eq(c.value, 0.0);
    }

    #[test]
    fn test_alias_conversion() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        // "meters" should resolve to "m"
        let res = converter.convert(1.0, "meters").unwrap();
        let cm = res.outputs.iter().find(|o| o.unit == "cm").unwrap();
        assert_relative_eq(cm.value, 100.0);
    }

    #[test]
    fn test_cross_currency_conversion() {
        let config = Config::default();
        let db = create_test_db();

        // 1. EUR is base
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();

        // 2. PLN (Fiat): 1 EUR = 4.0 PLN -> Factor = 1/4 = 0.25
        db.update_rate("PLN", 4.0, 1000, RateSource::Fiat).unwrap();

        // 3. BTC (Crypto): 1 BTC = 50000 EUR -> Factor = 50000
        db.update_rate("BTC", 50000.0, 1000, RateSource::Crypto)
            .unwrap();

        let converter = Converter::new(config, db);

        // Convert 1 BTC to PLN
        // Base_EUR = 1 * 50000 = 50000
        // Target_PLN = 50000 / 0.25 = 200000
        let res = converter.convert(1.0, "BTC").unwrap();
        let pln = res.outputs.iter().find(|o| o.unit == "PLN").unwrap();
        assert_relative_eq(pln.value, 200_000.0);

        // Convert 4 PLN to BTC
        // Base_EUR = 4 * 0.25 = 1.0
        // Target_BTC = 1.0 / 50000 = 0.00002
        let res = converter.convert(4.0, "PLN").unwrap();
        let btc = res.outputs.iter().find(|o| o.unit == "BTC").unwrap();
        assert_relative_eq(btc.value, 0.00002);
    }

    #[test]
    fn test_deduplication_and_sorting() {
        let config = Config {
            favorites: vec!["ft".to_string()],
            ..Config::default()
        };
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "m").unwrap();

        // Ensure "m" is not present in outputs when it's the input
        let m_count = res.outputs.iter().filter(|o| o.unit == "m").count();
        assert_eq!(m_count, 0, "Input unit should not be in output");

        // "ft" should be first because it's a favorite
        assert_eq!(res.outputs[0].unit, "ft");
    }

    #[test]
    fn test_currency_multipliers() {
        let config = Config::default();
        let db = create_test_db();
        db.update_unit("USD", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit("EUR", 0.9, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.5, "B USD").unwrap();
        assert_eq!(res.input_value, 1.5);
        assert_eq!(res.input_unit, "B USD");

        let eur = res.outputs.iter().find(|o| o.unit == "EUR").unwrap();
        // 1.5B USD -> 1,500,000,000 USD.
        // 1 USD = 1 Base
        // Base = 1.5e9.
        // EUR factor = 0.9.
        // Target_EUR = (1.5e9 / 0.9) = 1,666,666,666.66...
        assert!((eur.value - 1_666_666_666.6).abs() < 1.0);
    }

    #[test]
    fn test_metric_prefixes_fallback() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        // Convert 1 nanometer to cm
        // 1 nanometers -> actual_value = 1e-9, from_unit = "m"
        // 1e-9 m to cm -> Target = (1e-9 / 0.01) = 1e-7
        let res = converter.convert(1.0, "nanometers").unwrap();
        let cm = res.outputs.iter().find(|o| o.unit == "cm").unwrap();
        assert!((cm.value - 1e-7).abs() < 1e-10);
    }

    #[test]
    fn convert_preferring_should_pin_requested_unit_first() {
        let config = Config::default();
        let db = create_test_db();
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();
        db.update_unit("PLN", 0.25, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        let converter = Converter::new(config, db);

        let res = converter
            .convert_preferring(10.0, "EUR", Some("PLN"))
            .unwrap();
        assert_eq!(res.outputs[0].unit, "PLN");
        assert_relative_eq(res.outputs[0].value, 40.0);
        assert!(res.outputs.iter().any(|o| o.unit == "USD"));
    }

    #[test]
    fn convert_should_resolve_lowercase_currency_codes() {
        let config = Config::default();
        let db = create_test_db();
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit("USD", 0.5, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit("PLN", 0.25, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        let converter = Converter::new(config, db);

        // Plain lowercase source unit.
        let res = converter.convert(100.0, "usd").unwrap();
        assert!(res.outputs.iter().any(|o| o.unit == "EUR"));

        // Lowercase target clause pins the preferred unit.
        // Base = 100 * 0.5 = 50; Target_PLN = 50 / 0.25 = 200.
        let res = converter
            .convert_preferring(100.0, "usd", Some("pln"))
            .unwrap();
        assert_eq!(res.outputs[0].unit, "PLN");
        assert_relative_eq(res.outputs[0].value, 200.0);

        // Mixed case too.
        let res = converter.convert(1.0, "Eur").unwrap();
        assert!(res.outputs.iter().any(|o| o.unit == "USD"));
    }

    #[test]
    fn is_currency_unit_should_recognize_codes_case_insensitively() {
        let db = create_test_db();
        db.update_unit("USD", 0.5, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        let converter = Converter::new(Config::default(), db);

        assert!(converter.is_currency_unit("USD"));
        assert!(converter.is_currency_unit("usd"));
    }

    #[test]
    fn is_currency_unit_should_reject_non_currencies_and_unknown_symbols() {
        let db = create_test_db();
        let converter = Converter::new(Config::default(), db);

        assert!(!converter.is_currency_unit("m"));
        assert!(!converter.is_currency_unit("zzzz"));
    }

    #[test]
    fn convert_preferring_should_treat_unknown_preferred_units_as_no_target() {
        let config = Config::default();
        let db = create_test_db();
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit(
            "USD",
            1.0 / 1.1,
            0.0,
            UnitCategory::Currency,
            RateSource::Fiat,
        )
        .unwrap();
        let converter = Converter::new(config, db);

        // The default-target setting may hold a code that no longer resolves;
        // conversions must then behave exactly like an unpinned one.
        let res = converter
            .convert_preferring(10.0, "EUR", Some("ZZZZ"))
            .unwrap();
        assert!(res.outputs.iter().any(|o| o.unit == "USD"));
    }

    #[test]
    fn all_units_should_honor_disabled_packs() {
        let mut config = Config::default();
        config.unit_packs.volume = false;
        config.unit_packs.scientific = false;
        let db = create_test_db();
        let mut converter = Converter::new(config, db);

        let symbols: Vec<&str> = converter
            .all_units()
            .unwrap()
            .iter()
            .map(|u| u.info.symbol.as_str())
            .collect();
        assert!(!symbols.contains(&"L"));
        assert!(!symbols.contains(&"Pa"));
        assert!(symbols.contains(&"m"));
    }

    #[test]
    fn convert_should_still_work_for_disabled_pack_on_capture() {
        let mut config = Config::default();
        config.unit_packs.volume = false;
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "L").unwrap();
        assert!(res.outputs.iter().any(|o| o.unit == "gal"));
    }

    #[test]
    fn convert_should_handle_volume_and_speed() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "L").unwrap();
        let gal = res.outputs.iter().find(|o| o.unit == "gal").unwrap();
        assert!((gal.value - 1.0 / 3.785_411_784).abs() < 1e-9);

        let res = converter.convert(36.0, "km/h").unwrap();
        let mps = res.outputs.iter().find(|o| o.unit == "m/s").unwrap();
        assert!((mps.value - 10.0).abs() < 1e-9);
    }

    #[test]
    fn convert_should_handle_scientific_pressure() {
        let mut config = Config::default();
        config.unit_packs.scientific = true;
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter.convert(1.0, "bar").unwrap();
        let pa = res.outputs.iter().find(|o| o.unit == "Pa").unwrap();
        assert!((pa.value - 100_000.0).abs() < 1e-6);
    }

    #[test]
    fn convert_range_should_emit_one_row_per_endpoint_in_order() {
        let config = Config::default();
        let db = create_test_db();
        // EUR base (factor 1.0); 1 USD = 0.5 EUR.
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit("USD", 0.5, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        let converter = Converter::new(config, db);

        let parsed = ParsedInput {
            value: 10.0,
            end_value: Some(20.0),
            unit: Some("USD".to_string()),
            target: None,
            wall_clock: None,
        };
        let res = converter.convert_parsed(&parsed).unwrap();

        assert_eq!(res.input_value, 10.0);
        assert_eq!(res.outputs.len(), 2);
        // EUR is a favorite, so it leads both endpoint rows.
        assert_eq!(res.outputs[0].unit, "EUR");
        assert_relative_eq(res.outputs[0].value, 5.0);
        assert_eq!(res.outputs[1].unit, "EUR");
        assert_relative_eq(res.outputs[1].value, 10.0);
    }

    #[test]
    fn convert_parsed_should_convert_both_ends_of_parsed_dash_range() {
        let config = Config::default();
        let db = create_test_db();
        db.update_unit("EUR", 1.0, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        db.update_unit("USD", 0.5, 0.0, UnitCategory::Currency, RateSource::Fiat)
            .unwrap();
        let converter = Converter::new(config, db);

        let parsed = crate::parser::parse_input("10-20 USD").unwrap();
        let res = converter.convert_parsed(&parsed).unwrap();

        assert_eq!(res.outputs.len(), 2);
        assert_relative_eq(res.outputs[0].value, 5.0);
        assert_relative_eq(res.outputs[1].value, 10.0);
    }

    #[test]
    fn convert_range_rows_should_match_single_conversions_and_honor_target() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let parsed = crate::parser::parse_input("5-10 km to mi").unwrap();
        let res = converter.convert_parsed(&parsed).unwrap();

        let single_start = converter.convert_preferring(5.0, "km", Some("mi")).unwrap();
        let single_end = converter
            .convert_preferring(10.0, "km", Some("mi"))
            .unwrap();

        assert_eq!(res.outputs.len(), 2);
        assert_eq!(res.outputs[0].unit, "mi");
        assert_relative_eq(res.outputs[0].value, single_start.outputs[0].value);
        assert_eq!(res.outputs[1].unit, "mi");
        assert_relative_eq(res.outputs[1].value, single_end.outputs[0].value);
    }

    #[test]
    fn convert_range_should_collapse_degenerate_equal_endpoints_to_one_row() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let single = converter.convert(1.0, "m").unwrap();
        let parsed = ParsedInput {
            value: 1.0,
            end_value: Some(1.0),
            unit: Some("m".to_string()),
            target: None,
            wall_clock: None,
        };
        let res = converter.convert_parsed(&parsed).unwrap();

        assert_eq!(res.outputs.len(), 1);
        assert_eq!(res.outputs[0].value, single.outputs[0].value);
        assert_eq!(res.outputs[0].unit, single.outputs[0].unit);
    }

    #[test]
    fn convert_parsed_should_error_without_source_unit() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let parsed = ParsedInput {
            value: 5.0,
            end_value: None,
            unit: None,
            target: None,
            wall_clock: None,
        };
        assert!(converter.convert_parsed(&parsed).is_err());
    }

    #[test]
    fn convert_range_should_error_for_unknown_unit() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let parsed = ParsedInput {
            value: 1.0,
            end_value: Some(2.0),
            unit: Some("not-a-unit".to_string()),
            target: None,
            wall_clock: None,
        };
        assert!(converter.convert_parsed(&parsed).is_err());
    }

    /// 2026-01-15 12:00:00 UTC: northern-hemisphere winter, so fixed-offset
    /// expectations (CET +1, PST -8) are immune to DST politics.
    fn winter_noon_utc_epoch() -> i64 {
        Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    fn wall_clock_parsed(epoch_seconds: i64, target: Option<&str>) -> ParsedInput {
        ParsedInput {
            value: epoch_seconds as f64,
            end_value: None,
            unit: Some("2026-01-15T12:00Z".to_string()),
            target: target.map(str::to_string),
            wall_clock: Some(WallClock {
                epoch_seconds,
                source_zone: ResolvedZone::Utc,
            }),
        }
    }

    #[test]
    fn wall_clock_should_render_local_row_using_os_timezone() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let epoch = winter_noon_utc_epoch();
        let res = converter
            .convert_parsed(&wall_clock_parsed(epoch, None))
            .unwrap();

        // Anchor the assertion to chrono's own Local conversion instead of a
        // hardcoded zone so the test holds on any machine timezone.
        let expected_local = Utc
            .timestamp_opt(epoch, 0)
            .single()
            .unwrap()
            .with_timezone(&Local);
        let want_prefix = expected_local.format("%Y-%m-%d %H:%M").to_string();
        let local_row = res
            .outputs
            .iter()
            .find(|row| row.unit.contains("local"))
            .unwrap();
        assert!(local_row.unit.contains(&want_prefix), "{}", local_row.unit);
    }

    #[test]
    fn wall_clock_should_pin_explicit_iana_target_first() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter
            .convert_parsed(&wall_clock_parsed(
                winter_noon_utc_epoch(),
                Some("Europe/Warsaw"),
            ))
            .unwrap();

        assert_eq!(res.outputs.len(), 3);
        assert_eq!(res.outputs[0].value, winter_noon_utc_epoch() as f64);
        // 12:00 UTC == 13:00 CET in January.
        assert!(res.outputs[0].unit.contains("2026-01-15 13:00"));
        assert!(res.outputs[0].unit.contains("Europe/Warsaw"));
        assert!(res.outputs[1].unit.contains("local"));
        assert!(res.outputs[2].unit.ends_with("(UTC)"));
    }

    #[test]
    fn wall_clock_should_map_pst_abbreviation_to_a_representative_zone() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter
            .convert_parsed(&wall_clock_parsed(winter_noon_utc_epoch(), Some("pst")))
            .unwrap();

        // 12:00 UTC == 04:00 Pacific standard time; lowercase input too.
        assert!(
            res.outputs[0].unit.contains("2026-01-15 04:00"),
            "{}",
            res.outputs[0].unit
        );
        assert!(res.outputs[0].unit.contains("America/Los_Angeles"));
    }

    #[test]
    fn wall_clock_should_apply_fixed_offset_target() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter
            .convert_parsed(&wall_clock_parsed(winter_noon_utc_epoch(), Some("+05:30")))
            .unwrap();

        assert!(
            res.outputs[0].unit.contains("2026-01-15 17:30"),
            "{}",
            res.outputs[0].unit
        );
        assert!(res.outputs[0].unit.contains("(UTC+05:30)"));
    }

    #[test]
    fn wall_clock_should_error_for_unknown_target_zone() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let error = converter
            .convert_parsed(&wall_clock_parsed(
                winter_noon_utc_epoch(),
                Some("Mars/Olympus"),
            ))
            .unwrap_err();
        assert!(error.to_string().contains("Unknown timezone"), "{error}");
    }

    #[test]
    fn wall_clock_should_carry_the_epoch_on_every_row_for_copy() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let epoch = winter_noon_utc_epoch();
        let res = converter
            .convert_parsed(&wall_clock_parsed(epoch, None))
            .unwrap();

        // Enter/copy reads `ConvertedValue.value`, so datetime sources hand
        // back the epoch through the normal numeric copy path.
        assert!(res.outputs.iter().all(|row| row.value == epoch as f64));
        assert_eq!(res.input_value, epoch as f64);
        assert_eq!(res.input_unit, "2026-01-15T12:00Z");
    }

    #[test]
    fn convert_parsed_should_dispatch_wall_clock_without_unit_text() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let parsed = ParsedInput {
            value: 0.0,
            end_value: None,
            unit: None,
            target: None,
            wall_clock: Some(WallClock {
                epoch_seconds: winter_noon_utc_epoch(),
                source_zone: ResolvedZone::Utc,
            }),
        };
        let res = converter.convert_parsed(&parsed).unwrap();
        assert!(res.outputs.len() >= 2);
    }

    #[test]
    fn wall_clock_should_collapse_duplicate_rows_when_target_is_utc() {
        let config = Config::default();
        let db = create_test_db();
        let converter = Converter::new(config, db);

        let res = converter
            .convert_parsed(&wall_clock_parsed(winter_noon_utc_epoch(), Some("UTC")))
            .unwrap();

        // Pinned UTC row and the trailing reference row merge into one.
        assert_eq!(res.outputs.len(), 2);
        assert!(res.outputs.iter().any(|row| row.unit.ends_with("(UTC)")));
        assert!(res.outputs.iter().any(|row| row.unit.contains("local")));
    }
}
