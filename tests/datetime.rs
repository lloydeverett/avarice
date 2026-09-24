//! The `datetime` module (ADR 0019), driven the way a script does: Lua through a trusted runtime.
//!
//! The module mirrors jiff, so these check the binding (what reaches Lua, what raises, what each
//! operator means) and lean on jiff for the calendar arithmetic itself. Every expected value is a
//! literal worked out by hand, or taken from jiff's own documentation.

#![cfg(feature = "stdlib-datetime")]

use avarice_rt::{Profile, Runtime};

/// Runs `body` with `dt` bound to the module, and returns what it returns.
fn eval<R: mlua::FromLuaMulti>(body: &str) -> R {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let source = format!("local dt = require('datetime')\n{body}");
    rt.block_on(rt.eval(source, "=test"))
        .unwrap_or_else(|e| panic!("{body}: {e}"))
}

/// Runs `body` with `dt` bound to the module, expecting it to raise, and returns the message.
fn raises(body: &str) -> String {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    let source = format!(
        "local dt = require('datetime')\n\
         local ok, err = pcall(function() {body} end)\n\
         assert(not ok, 'did not raise')\n\
         return tostring(err)"
    );
    rt.block_on(rt.eval::<String>(source, "=test"))
        .unwrap_or_else(|e| panic!("{body}: {e}"))
}

// -- Date --------------------------------------------------------------------------------------

#[test]
fn a_date_is_built_from_its_fields_and_prints_as_iso_8601() {
    let got: String = eval("return tostring(dt.date(2024, 1, 2))");
    assert_eq!(got, "2024-01-02");
}

#[test]
fn a_date_parses_what_it_prints() {
    let got: bool = eval("local d = dt.date(2024, 2, 29) return dt.Date.parse(tostring(d)) == d");
    assert!(got);
}

#[test]
fn a_date_reads_its_fields_and_weekday() {
    let got: (i64, i64, i64, String, i64) = eval(
        "local d = dt.date(2024, 7, 11)
         return d:year(), d:month(), d:day(), tostring(d:weekday()), d:day_of_year()",
    );
    assert_eq!(got, (2024, 7, 11, "Thursday".into(), 193));
}

#[test]
fn an_impossible_date_raises() {
    assert!(raises("dt.date(2023, 2, 29)").contains("day"));
}

#[test]
fn a_fractional_argument_raises_and_a_whole_float_is_an_integer() {
    assert!(raises("dt.date(2024, 1.5, 1)").contains("expected an integer, got 1.5"));
    let got: String = eval("return tostring(dt.date(2024.0, 1, 2))");
    assert_eq!(got, "2024-01-02");
}

#[test]
fn a_string_is_not_taken_for_a_number() {
    assert!(raises("dt.date('2024', 1, 1)").contains("expected an integer"));
}

#[test]
fn with_returns_a_changed_copy_and_leaves_the_original() {
    let got: (String, String) = eval(
        "local d = dt.date(2024, 1, 31)
         local e = d:with { month = 2, day = 29 }
         return tostring(d), tostring(e)",
    );
    assert_eq!(got, ("2024-01-31".into(), "2024-02-29".into()));
}

#[test]
fn with_an_unknown_key_raises() {
    assert!(raises("dt.date(2024, 1, 1):with { dya = 2 }").contains("unknown key 'dya'"));
}

#[test]
fn date_moves_find_the_ends_of_months_and_nth_weekdays() {
    let got: (String, String, String) = eval(
        "local d = dt.date(2024, 2, 10)
         return tostring(d:last_of_month()), tostring(d:tomorrow()),
                tostring(d:nth_weekday_of_month(-1, 'friday'))",
    );
    assert_eq!(
        got,
        (
            "2024-02-29".into(),
            "2024-02-11".into(),
            "2024-02-23".into()
        )
    );
}

#[test]
fn a_date_formats_and_parses_with_strftime_and_strptime() {
    let got: (String, String) = eval(
        "local d = dt.date(2024, 7, 14)
         return d:strftime('%a %d %b %Y'), tostring(dt.Date.strptime('%d/%m/%Y', '14/07/2024'))",
    );
    assert_eq!(got, ("Sun 14 Jul 2024".into(), "2024-07-14".into()));
}

#[test]
fn a_bad_strftime_directive_raises() {
    raises("dt.date(2024, 1, 1):strftime('%Q')");
}

// -- Time and DateTime -------------------------------------------------------------------------

#[test]
fn a_time_defaults_its_seconds_and_nanoseconds() {
    let got: (String, String) =
        eval("return tostring(dt.time(9, 30)), tostring(dt.time(9, 30, 5, 123000000))");
    assert_eq!(got, ("09:30:00".into(), "09:30:05.123".into()));
}

#[test]
fn a_date_at_a_time_is_a_datetime() {
    let got: String = eval("return tostring(dt.date(2024, 1, 2):at(3, 4, 5))");
    assert_eq!(got, "2024-01-02T03:04:05");
}

#[test]
fn a_datetime_is_built_from_civil_fields() {
    let got: (String, String) = eval(
        "return tostring(dt.datetime(2024, 1, 2, 3, 4, 5)), tostring(dt.datetime(2024, 1, 2))",
    );
    assert_eq!(
        got,
        ("2024-01-02T03:04:05".into(), "2024-01-02T00:00:00".into())
    );
}

#[test]
fn a_datetime_rounds_to_a_unit() {
    let got: (String, String) = eval(
        "local d = dt.datetime(2024, 6, 20, 3, 25, 1)
         return tostring(d:round('hour')), tostring(d:round { smallest = 'minute', increment = 30 })",
    );
    assert_eq!(
        got,
        ("2024-06-20T03:00:00".into(), "2024-06-20T03:30:00".into())
    );
}

#[test]
fn an_unknown_unit_or_mode_raises() {
    assert!(raises("dt.time(1, 2):round('fortnight')").contains("unknown unit"));
    assert!(
        raises("dt.time(1, 2):round { smallest = 'hour', mode = 'up' }").contains("unknown mode")
    );
}

// -- Timestamp ---------------------------------------------------------------------------------

#[test]
fn a_timestamp_parses_rfc_3339_and_prints_in_utc() {
    let got: (String, i64) = eval(
        "local t = dt.Timestamp.parse('2024-01-02T03:04:05+01:00')
         return tostring(t), t:as_second()",
    );
    assert_eq!(got, ("2024-01-02T02:04:05Z".into(), 1_704_161_045));
}

#[test]
fn a_timestamp_is_built_from_units_since_the_epoch() {
    let got: (String, String) = eval(
        "return tostring(dt.Timestamp.from_second(0)),
                tostring(dt.Timestamp.from_millisecond(1500))",
    );
    assert_eq!(
        got,
        (
            "1970-01-01T00:00:00Z".into(),
            "1970-01-01T00:00:01.5Z".into()
        )
    );
}

#[test]
fn now_is_a_timestamp_and_a_zoned() {
    let got: (bool, bool) = eval(
        "return dt.Timestamp.now() > dt.Timestamp.from_second(0),
                dt.Zoned.now():timestamp() > dt.Timestamp.from_second(0)",
    );
    assert_eq!(got, (true, true));
}

#[test]
fn a_timestamp_in_a_zone_is_a_zoned() {
    let got: String =
        eval("return tostring(dt.Timestamp.from_second(0):in_tz('America/New_York'))");
    assert_eq!(got, "1969-12-31T19:00:00-05:00[America/New_York]");
}

#[test]
fn a_bad_parse_raises() {
    raises("dt.Timestamp.parse('yesterday')");
    raises("dt.Date.parse('2024-13-01')");
}

// -- Zoned and TimeZone ------------------------------------------------------------------------

#[test]
fn a_zoned_parses_with_its_zone_and_reads_its_fields() {
    let got: (String, i64, String) = eval(
        "local z = dt.Zoned.parse('2024-03-10T01:30:00-05:00[America/New_York]')
         return tostring(z), z:hour(), tostring(z:time_zone())",
    );
    assert_eq!(
        got,
        (
            "2024-03-10T01:30:00-05:00[America/New_York]".into(),
            1,
            "America/New_York".into()
        )
    );
}

#[test]
fn adding_a_day_across_a_dst_change_keeps_the_wall_clock() {
    // 2024-03-10 is when New York springs forward: a day later is 23 hours later.
    let got: (String, String) = eval(
        "local z = dt.Zoned.parse('2024-03-09T12:00:00-05:00[America/New_York]')
         local later = z + dt.span { days = 1 }
         return tostring(later), tostring(later - z)",
    );
    assert_eq!(
        got,
        (
            "2024-03-10T12:00:00-04:00[America/New_York]".into(),
            "PT23H".into()
        )
    );
}

#[test]
fn a_time_in_a_dst_gap_is_resolved_compatibly_by_default() {
    // 02:30 on 2024-03-10 does not exist in New York; compatible moves it forward an hour.
    let got: String =
        eval("return tostring(dt.datetime(2024, 3, 10, 2, 30):in_tz('America/New_York'))");
    assert_eq!(got, "2024-03-10T03:30:00-04:00[America/New_York]");
}

#[test]
fn a_time_in_a_dst_gap_raises_when_asked_to_reject() {
    let tz = "local tz = dt.TimeZone.get('America/New_York') ";
    raises(&format!(
        "{tz} dt.datetime(2024, 3, 10, 2, 30):to_zoned(tz, {{ disambiguation = 'reject' }})"
    ));
    raises(
        "dt.datetime(2024, 3, 10, 2, 30):in_tz('America/New_York', { disambiguation = 'reject' })",
    );
}

#[test]
fn an_ambiguous_time_takes_the_earlier_or_later_offset() {
    // 01:30 on 2024-11-03 happens twice in New York.
    let got: (String, String) = eval(
        "local d = dt.datetime(2024, 11, 3, 1, 30)
         return tostring(d:in_tz('America/New_York', { disambiguation = 'earlier' })),
                tostring(d:in_tz('America/New_York', { disambiguation = 'later' }))",
    );
    assert_eq!(
        got,
        (
            "2024-11-03T01:30:00-04:00[America/New_York]".into(),
            "2024-11-03T01:30:00-05:00[America/New_York]".into()
        )
    );
}

#[test]
fn an_unknown_time_zone_raises() {
    raises("dt.TimeZone.get('Mars/Olympus_Mons')");
}

#[test]
fn the_system_zone_and_utc_are_time_zones() {
    let got: (String, bool) = eval("return tostring(dt.TimeZone.UTC), dt.TimeZone.system() ~= nil");
    assert_eq!(got, ("UTC".into(), true));
}

#[test]
fn a_zoned_until_another_takes_the_largest_unit_asked_for() {
    let got: String = eval(
        "local a = dt.Zoned.parse('2024-01-15T00:00:00+00:00[UTC]')
         local b = dt.Zoned.parse('2025-03-20T06:00:00+00:00[UTC]')
         return tostring(a:span_until(b, { largest = 'year', smallest = 'day' }))",
    );
    assert_eq!(got, "P1Y2M5D");
}

#[test]
fn a_zoned_with_a_field_changed_stays_in_its_zone() {
    let got: String = eval(
        "local z = dt.Zoned.parse('2024-07-04T12:00:00-04:00[America/New_York]')
         return tostring(z:with { month = 1 })",
    );
    assert_eq!(got, "2024-01-04T12:00:00-05:00[America/New_York]");
}

// -- Weekday -----------------------------------------------------------------------------------

#[test]
fn a_weekday_is_a_value_with_a_name_and_offsets() {
    let got: (String, i64, i64, bool) = eval(
        "local w = dt.Weekday.sunday
         return tostring(w), w:to_monday_one_offset(), w:to_sunday_zero_offset(),
                w == dt.Weekday.from_name('SUN')",
    );
    assert_eq!(got, ("Sunday".into(), 7, 0, true));
}

#[test]
fn a_weekday_counts_days_to_another_and_wraps() {
    let got: (i64, String) =
        eval("return dt.Weekday.friday:days_until('monday'), tostring(dt.Weekday.sunday:next())");
    assert_eq!(got, (3, "Monday".into()));
}

#[test]
fn an_unknown_weekday_name_raises() {
    assert!(raises("dt.Weekday.from_name('Funday')").contains("not a weekday"));
}

// -- Span --------------------------------------------------------------------------------------

#[test]
fn a_span_is_built_from_a_table_and_read_back() {
    let got: (String, i64, i64) = eval(
        "local s = dt.span { days = 5, hours = 3 }
         return tostring(s), s:get_days(), s:hours(4):get_hours()",
    );
    assert_eq!(got, ("P5DT3H".into(), 5, 4));
}

#[test]
fn a_span_parses_the_friendly_format() {
    let got: String = eval("return tostring(dt.Span.parse('5 days 3 hours'))");
    assert_eq!(got, "P5DT3H");
}

#[test]
fn span_equality_is_fieldwise_and_compare_is_by_length() {
    let got: (bool, i64) = eval(
        "local a, b = dt.span { hours = 1 }, dt.span { minutes = 60 }
         return a == b, a:compare(b)",
    );
    assert_eq!(got, (false, 0));
}

#[test]
fn spans_with_calendar_units_need_a_relative_date_to_add_or_total() {
    let got: (String, f64) = eval(
        "local month = dt.span { months = 1 }
         local r = { relative = dt.date(2024, 2, 1) }
         return tostring(month:add(dt.span { days = 1 }, r)), month:total('day', r)",
    );
    assert_eq!(got, ("P1M1D".into(), 29.0));
    raises("dt.span { months = 1 }:total('day')");
}

#[test]
fn span_plus_span_and_span_less_than_span_raise() {
    assert!(raises("return dt.span { hours = 1 } + dt.span { hours = 1 }").contains("span:add"));
    assert!(raises("return dt.span { hours = 1 } < dt.span { hours = 2 }").contains("compare"));
}

#[test]
fn a_span_negates_and_scales() {
    let got: (String, String, String) = eval(
        "local s = dt.span { hours = 2 }
         return tostring(-s), tostring(s * 3), tostring(3 * s)",
    );
    assert_eq!(got, ("-PT2H".into(), "PT6H".into(), "PT6H".into()));
}

// -- SignedDuration ----------------------------------------------------------------------------

#[test]
fn signed_durations_do_full_arithmetic() {
    let got: (String, String, String, String, bool) = eval(
        "local a, b = dt.SignedDuration.from_secs(90), dt.SignedDuration.from_millis(500)
         return tostring(a + b), tostring(a - b), tostring(a * 2), tostring(a / 4), b < a",
    );
    assert_eq!(
        got,
        (
            "PT1M30.5S".into(),
            "PT1M29.5S".into(),
            "PT3M".into(),
            "PT22.5S".into(),
            true
        )
    );
}

#[test]
fn a_signed_duration_converts_to_and_from_floats() {
    let got: (f64, i64) = eval(
        "return dt.SignedDuration.from_secs_f64(1.25):as_secs_f64(),
                dt.SignedDuration.parse('2h 30m'):as_mins()",
    );
    assert_eq!(got, (1.25, 150));
}

#[test]
fn a_signed_duration_divided_by_zero_raises() {
    assert!(raises("return dt.SignedDuration.from_secs(1) / 0").contains("division by zero"));
}

// -- Operators across types --------------------------------------------------------------------

#[test]
fn a_value_plus_a_span_or_duration_is_the_same_type() {
    let got: (String, String, String) = eval(
        "local d = dt.date(2024, 1, 31)
         return tostring(d + dt.span { months = 1 }), tostring(d - dt.span { days = 31 }),
                tostring(dt.time(23, 0) + dt.SignedDuration.from_mins(30))",
    );
    assert_eq!(
        got,
        ("2024-02-29".into(), "2023-12-31".into(), "23:30:00".into())
    );
}

#[test]
fn a_value_minus_one_of_its_type_is_a_span() {
    let got: String = eval("return tostring(dt.date(2024, 3, 1) - dt.date(2024, 1, 1))");
    assert_eq!(got, "P60D");
}

#[test]
fn values_order_within_a_type() {
    let got: (bool, bool, bool) = eval(
        "local a, b = dt.date(2024, 1, 1), dt.date(2024, 1, 2)
         return a < b, b <= b, a == dt.date(2024, 1, 1)",
    );
    assert_eq!(got, (true, true, true));
}

#[test]
fn ordering_across_types_raises_and_equality_is_false() {
    assert!(
        raises("return dt.date(2024, 1, 1) < dt.datetime(2024, 1, 2)").contains("cannot compare")
    );
    let got: bool = eval("return dt.date(2024, 1, 1) == dt.datetime(2024, 1, 1)");
    assert!(!got);
}

#[test]
fn adding_something_that_is_not_a_span_raises() {
    assert!(
        raises("return dt.date(2024, 1, 1) + 1").contains("expected a Span or a SignedDuration")
    );
}

// -- sleep -------------------------------------------------------------------------------------

#[test]
fn sleep_takes_milliseconds_a_duration_or_a_clock_span() {
    let start = std::time::Instant::now();
    let _: () = eval(
        "dt.sleep(20)
         dt.sleep(dt.SignedDuration.from_millis(20))
         dt.sleep(dt.span { milliseconds = 20 })",
    );
    assert!(start.elapsed() >= std::time::Duration::from_millis(60));
}

#[test]
fn sleep_raises_on_a_fraction_a_negative_or_a_calendar_span() {
    assert!(raises("dt.sleep(1.5)").contains("expected an integer"));
    assert!(raises("dt.sleep(-1)").contains("negative"));
    assert!(raises("dt.sleep(dt.span { days = 1 })").contains("no fixed length"));
}

// -- The module --------------------------------------------------------------------------------

#[test]
fn requiring_it_sets_no_globals() {
    // The Rust half is handed to the Lua file as a value (ADR 0019), not left in the globals.
    let got: i64 = eval(
        "local n = 0
         for k in pairs(_G) do if tostring(k):find('datetime') then n = n + 1 end end
         return n",
    );
    assert_eq!(got, 0);
}
