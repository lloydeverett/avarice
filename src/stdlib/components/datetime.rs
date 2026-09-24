//! `datetime` is original to avarice-rt, not derived from Astra: ADR 0019 records why. It binds
//! [jiff](https://docs.rs/jiff) to Lua, one userdata per jiff type: `Timestamp`, `Zoned`, `Date`,
//! `Time`, `DateTime`, `Span`, `SignedDuration`, `TimeZone` and `Weekday`.
//!
//! The binding is thin and follows jiff's names, with five Lua-shaped differences. Every method that
//! jiff spells `checked_*` drops the prefix, because everything here raises on failure (ADR 0017).
//! jiff's builders become an optional options table. `with` takes a table of fields. Operators are
//! metamethods. And `until`, a Lua keyword, cannot be a method name, so jiff's `until` and `since`
//! are `span_until` and `span_since` (and a Weekday's are `days_until` and `days_since`). Numbers
//! are integers, and a float with a fractional part raises, except where jiff itself takes or gives
//! an `f64`.
//!
//! Nothing here sets a global. [`rust_half`] builds a table of the types' constructors, and `modules.rs`
//! hands it to `lua/datetime.lua`, which returns it as the module.

use std::fmt::Display;

use jiff::civil;
use jiff::tz::{self, Disambiguation};
use jiff::{RoundMode, Unit};
use mlua::{Lua, MetaMethod, Table, UserData, UserDataMethods, Value};

type Result<T> = mlua::Result<T>;

/// Builds the table `lua/datetime.lua` returns: one table of constructors per type, the
/// shorthands, and `sleep`.
pub fn rust_half(lua: &Lua) -> Result<Value> {
    let module = lua.create_table()?;
    module.set("Timestamp", timestamp_table(lua)?)?;
    module.set("Zoned", zoned_table(lua)?)?;
    module.set("Date", date_table(lua)?)?;
    module.set("Time", time_table(lua)?)?;
    module.set("DateTime", datetime_table(lua)?)?;
    module.set("Span", span_table(lua)?)?;
    module.set("SignedDuration", signed_duration_table(lua)?)?;
    module.set("TimeZone", time_zone_table(lua)?)?;
    module.set("Weekday", weekday_table(lua)?)?;
    module.set("date", lua.create_function(|_, args| new_date(args))?)?;
    module.set("time", lua.create_function(|_, args| new_time(args))?)?;
    module.set(
        "datetime",
        lua.create_function(|_, args| new_datetime(args))?,
    )?;
    module.set(
        "span",
        lua.create_function(|_, fields: Value| {
            span_with(jiff::Span::new(), &table_arg(&fields, "datetime.span")?).map(Span)
        })?,
    )?;
    module.set(
        "sleep",
        lua.create_async_function(|_, amount: Value| {
            let amount = sleep_duration(&amount);
            async move {
                tokio::time::sleep(amount?).await;
                Ok(())
            }
        })?,
    )?;
    Ok(Value::Table(module))
}

// -- Arguments ---------------------------------------------------------------------------------

fn raise(message: impl Display) -> mlua::Error {
    mlua::Error::runtime(message.to_string())
}

/// What went wrong in jiff, raised as a Lua error.
trait OrRaise<T> {
    fn or_raise(self) -> Result<T>;
}

impl<T> OrRaise<T> for std::result::Result<T, jiff::Error> {
    fn or_raise(self) -> Result<T> {
        self.map_err(raise)
    }
}

/// How a value reads in an error message: its type, and the value itself when that is short.
fn describe(value: &Value) -> String {
    match value {
        Value::Nil => "nil".into(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::String(s) => format!("\"{}\"", s.to_string_lossy()),
        Value::UserData(ud) => ud
            .type_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|_| "userdata".into()),
        other => other.type_name().into(),
    }
}

/// `value` as an integer. A float counts if it has no fractional part; a string does not, though
/// Lua would coerce one, because a date is not arithmetic on text.
fn integer(value: &Value, what: &str) -> Result<i64> {
    match *value {
        Value::Integer(i) => Ok(i),
        Value::Number(n) if n.fract() == 0.0 && n >= -(2f64.powi(63)) && n < 2f64.powi(63) => {
            Ok(n as i64)
        }
        _ => Err(raise(format!(
            "{what}: expected an integer, got {}",
            describe(value)
        ))),
    }
}

/// `value` as an integer that fits `T`: jiff's fields are `i8`, `i16` and `i32`.
fn narrow<T: TryFrom<i64>>(value: &Value, what: &str) -> Result<T> {
    let n = integer(value, what)?;
    T::try_from(n).map_err(|_| raise(format!("{what}: {n} is out of range")))
}

/// Like [`narrow`], but `nil` is `default`: for trailing arguments that may be left off.
fn narrow_or<T: TryFrom<i64>>(value: &Value, default: T, what: &str) -> Result<T> {
    match value {
        Value::Nil => Ok(default),
        value => narrow(value, what),
    }
}

fn float(value: &Value, what: &str) -> Result<f64> {
    match *value {
        Value::Integer(i) => Ok(i as f64),
        Value::Number(n) => Ok(n),
        _ => Err(raise(format!(
            "{what}: expected a number, got {}",
            describe(value)
        ))),
    }
}

fn string(value: &Value, what: &str) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.to_str()?.to_owned()),
        _ => Err(raise(format!(
            "{what}: expected a string, got {}",
            describe(value)
        ))),
    }
}

fn table_arg(value: &Value, what: &str) -> Result<Table> {
    match value {
        Value::Table(t) => Ok(t.clone()),
        _ => Err(raise(format!(
            "{what}: expected a table, got {}",
            describe(value)
        ))),
    }
}

/// `value` as one of this module's userdata types, by value.
fn userdata<T: UserData + Clone + 'static>(value: &Value, what: &str) -> Result<T> {
    if let Value::UserData(ud) = value
        && let Ok(inner) = ud.borrow::<T>()
    {
        return Ok(inner.clone());
    }
    Err(raise(format!(
        "{what}: expected a {}, got {}",
        short_name::<T>(),
        describe(value)
    )))
}

fn is<T: UserData + 'static>(value: &Value) -> bool {
    matches!(value, Value::UserData(ud) if ud.is::<T>())
}

fn short_name<T>() -> &'static str {
    let name = std::any::type_name::<T>();
    name.rsplit("::").next().unwrap_or(name)
}

/// Walks an options or fields table, handing each key to `field`, which returns `false` for a
/// key it does not know. An unknown key raises, naming the ones that are known: a misspelt
/// option is a mistake, not an option left out.
fn each_field(
    table: &Table,
    what: &str,
    known: &[&str],
    mut field: impl FnMut(&str, Value) -> Result<bool>,
) -> Result<()> {
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let name = match &key {
            Value::String(s) => s.to_str()?.to_owned(),
            other => {
                return Err(raise(format!(
                    "{what}: keys must be strings, got {}",
                    describe(other)
                )));
            }
        };
        if !field(&name, value)? {
            return Err(raise(format!(
                "{what}: unknown key '{name}' (expected one of: {})",
                known.join(", ")
            )));
        }
    }
    Ok(())
}

fn unit(value: &Value, what: &str) -> Result<Unit> {
    Ok(match string(value, what)?.as_str() {
        "year" => Unit::Year,
        "month" => Unit::Month,
        "week" => Unit::Week,
        "day" => Unit::Day,
        "hour" => Unit::Hour,
        "minute" => Unit::Minute,
        "second" => Unit::Second,
        "millisecond" => Unit::Millisecond,
        "microsecond" => Unit::Microsecond,
        "nanosecond" => Unit::Nanosecond,
        other => {
            return Err(raise(format!(
                "{what}: unknown unit \"{other}\" (expected year, month, week, day, hour, \
                 minute, second, millisecond, microsecond or nanosecond)"
            )));
        }
    })
}

fn round_mode(value: &Value, what: &str) -> Result<RoundMode> {
    Ok(match string(value, what)?.as_str() {
        "ceil" => RoundMode::Ceil,
        "floor" => RoundMode::Floor,
        "expand" => RoundMode::Expand,
        "trunc" => RoundMode::Trunc,
        "half_ceil" => RoundMode::HalfCeil,
        "half_floor" => RoundMode::HalfFloor,
        "half_expand" => RoundMode::HalfExpand,
        "half_trunc" => RoundMode::HalfTrunc,
        "half_even" => RoundMode::HalfEven,
        other => {
            return Err(raise(format!(
                "{what}: unknown mode \"{other}\" (expected ceil, floor, expand, trunc, \
                 half_ceil, half_floor, half_expand, half_trunc or half_even)"
            )));
        }
    })
}

fn disambiguation(value: &Value, what: &str) -> Result<Disambiguation> {
    Ok(match string(value, what)?.as_str() {
        "compatible" => Disambiguation::Compatible,
        "earlier" => Disambiguation::Earlier,
        "later" => Disambiguation::Later,
        "reject" => Disambiguation::Reject,
        other => {
            return Err(raise(format!(
                "{what}: unknown disambiguation \"{other}\" (expected compatible, earlier, \
                 later or reject)"
            )));
        }
    })
}

/// A span or a signed duration: what can be added to or subtracted from a value.
enum Duration {
    Span(jiff::Span),
    Signed(jiff::SignedDuration),
}

fn duration(value: &Value, what: &str) -> Result<Duration> {
    if let Value::UserData(ud) = value {
        if let Ok(span) = ud.borrow::<Span>() {
            return Ok(Duration::Span(span.0));
        }
        if let Ok(duration) = ud.borrow::<SignedDuration>() {
            return Ok(Duration::Signed(duration.0));
        }
    }
    Err(raise(format!(
        "{what}: expected a Span or a SignedDuration, got {}",
        describe(value)
    )))
}

/// The options `span_until` and `span_since` take: jiff's `*Difference` builders, as a table.
#[derive(Default)]
struct Difference {
    largest: Option<Unit>,
    smallest: Option<Unit>,
    mode: Option<RoundMode>,
    increment: Option<i64>,
}

const DIFFERENCE_KEYS: &[&str] = &["largest", "smallest", "mode", "increment"];

fn difference(options: &Value, what: &str) -> Result<Difference> {
    let mut d = Difference::default();
    if options.is_nil() {
        return Ok(d);
    }
    each_field(
        &table_arg(options, what)?,
        what,
        DIFFERENCE_KEYS,
        |key, v| {
            match key {
                "largest" => d.largest = Some(unit(&v, what)?),
                "smallest" => d.smallest = Some(unit(&v, what)?),
                "mode" => d.mode = Some(round_mode(&v, what)?),
                "increment" => d.increment = Some(integer(&v, what)?),
                _ => return Ok(false),
            }
            Ok(true)
        },
    )?;
    Ok(d)
}

/// Applies a [`Difference`] to one of jiff's `*Difference` builders.
macro_rules! configure {
    ($builder:expr, $d:expr) => {{
        let mut b = $builder;
        if let Some(u) = $d.largest {
            b = b.largest(u);
        }
        if let Some(u) = $d.smallest {
            b = b.smallest(u);
        }
        if let Some(m) = $d.mode {
            b = b.mode(m);
        }
        if let Some(i) = $d.increment {
            b = b.increment(i);
        }
        b
    }};
}

/// The options `round` takes: a unit on its own, or a table of `smallest`, `mode` and `increment`.
#[derive(Default)]
struct Round {
    smallest: Option<Unit>,
    mode: Option<RoundMode>,
    increment: Option<i64>,
}

fn round(options: &Value, what: &str) -> Result<Round> {
    if let Value::String(_) = options {
        return Ok(Round {
            smallest: Some(unit(options, what)?),
            ..Round::default()
        });
    }
    let mut r = Round::default();
    each_field(
        &table_arg(options, what)?,
        what,
        &["smallest", "mode", "increment"],
        |key, v| {
            match key {
                "smallest" => r.smallest = Some(unit(&v, what)?),
                "mode" => r.mode = Some(round_mode(&v, what)?),
                "increment" => r.increment = Some(integer(&v, what)?),
                _ => return Ok(false),
            }
            Ok(true)
        },
    )?;
    Ok(r)
}

macro_rules! configure_round {
    ($builder:expr, $r:expr) => {{
        let mut b = $builder;
        if let Some(u) = $r.smallest {
            b = b.smallest(u);
        }
        if let Some(m) = $r.mode {
            b = b.mode(m);
        }
        if let Some(i) = $r.increment {
            b = b.increment(i);
        }
        b
    }};
}

/// The `{ disambiguation = ... }` table a civil-to-zoned conversion takes.
fn disambiguation_option(options: &Value, what: &str) -> Result<Disambiguation> {
    let mut d = Disambiguation::Compatible;
    if options.is_nil() {
        return Ok(d);
    }
    each_field(
        &table_arg(options, what)?,
        what,
        &["disambiguation"],
        |key, v| {
            if key != "disambiguation" {
                return Ok(false);
            }
            d = disambiguation(&v, what)?;
            Ok(true)
        },
    )?;
    Ok(d)
}

fn to_zoned(
    zone: &jiff::tz::TimeZone,
    dt: civil::DateTime,
    options: &Value,
    what: &str,
) -> Result<jiff::Zoned> {
    let d = disambiguation_option(options, what)?;
    zone.to_ambiguous_zoned(dt).disambiguate(d).or_raise()
}

fn in_tz(name: &Value, dt: civil::DateTime, options: &Value, what: &str) -> Result<jiff::Zoned> {
    let zone = jiff::tz::TimeZone::get(&string(name, what)?).or_raise()?;
    to_zoned(&zone, dt, options, what)
}

fn weekday(value: &Value, what: &str) -> Result<civil::Weekday> {
    match value {
        Value::String(s) => weekday_from_name(&s.to_str()?, what),
        other => userdata::<Weekday>(other, what).map(|w| w.0),
    }
}

fn i128_to_i64(n: i128, what: &str) -> Result<i64> {
    i64::try_from(n).map_err(|_| raise(format!("{what}: {n} does not fit in a Lua integer")))
}

/// jiff's `Display` for a strftime format, which fails on a bad directive rather than panicking.
fn strftime(format: &Value, time: impl Into<jiff::fmt::strtime::BrokenDownTime>) -> Result<String> {
    jiff::fmt::strtime::format(string(format, "strftime")?, time).or_raise()
}

fn parse<T: std::str::FromStr<Err = jiff::Error>>(text: &Value, what: &str) -> Result<T> {
    string(text, what)?.parse().or_raise()
}

// -- Shared methods ----------------------------------------------------------------------------

/// `__tostring` as jiff's `Display`, `==`, and, when `$ordered`, `<` and `<=` within the type.
/// Comparing with another type raises for an ordering and is `false` for `==`, which is all Lua
/// lets `__eq` say.
macro_rules! display_and_compare {
    ($methods:ident, $ty:ident) => {
        $methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.0.to_string()));
        $methods.add_meta_method(MetaMethod::Eq, |_, this, other: Value| {
            Ok(match &other {
                Value::UserData(ud) => ud.borrow::<$ty>().is_ok_and(|o| o.0 == this.0),
                _ => false,
            })
        });
    };
    ($methods:ident, $ty:ident, ordered) => {
        display_and_compare!($methods, $ty);
        $methods.add_meta_function(MetaMethod::Lt, |_, (a, b): (Value, Value)| {
            let (a, b) = ordered::<$ty>(&a, &b, "<")?;
            Ok(a.0 < b.0)
        });
        $methods.add_meta_function(MetaMethod::Le, |_, (a, b): (Value, Value)| {
            let (a, b) = ordered::<$ty>(&a, &b, "<=")?;
            Ok(a.0 <= b.0)
        });
    };
}

fn ordered<T: UserData + Clone + 'static>(a: &Value, b: &Value, op: &str) -> Result<(T, T)> {
    if is::<T>(a) && is::<T>(b) {
        return Ok((userdata(a, op)?, userdata(b, op)?));
    }
    Err(raise(format!(
        "cannot compare {} {op} {}: only values of the same type are ordered",
        describe(a),
        describe(b)
    )))
}

/// `add`, `sub`, `span_until`, `span_since`, `duration_until`, `duration_since`, `strftime`, and
/// the `+` and `-` operators: `value ± span` is a value, and `value - value` is a span.
///
/// jiff's `until` and `since` are `span_until` and `span_since` here, because `until` is a Lua
/// keyword and `a:until(b)` does not parse. The names pair with `duration_until` and
/// `duration_since`, which give a SignedDuration where these give a Span.
macro_rules! arithmetic {
    ($methods:ident, $ty:ident, $difference:path) => {
        $methods.add_method("add", |_, this, d: Value| add::<$ty>(&this.0, &d, "add"));
        $methods.add_method("sub", |_, this, d: Value| sub::<$ty>(&this.0, &d, "sub"));
        $methods.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            let a = userdata::<$ty>(&a, concat!(stringify!($ty), " +"))?;
            add::<$ty>(&a.0, &b, concat!(stringify!($ty), " +"))
        });
        $methods.add_meta_function(MetaMethod::Sub, |lua, (a, b): (Value, Value)| {
            let what = concat!(stringify!($ty), " -");
            let a = userdata::<$ty>(&a, what)?;
            if is::<$ty>(&b) {
                let b = userdata::<$ty>(&b, what)?;
                return Span(a.0.since(b.arg()).or_raise()?).into_lua_value(lua);
            }
            sub::<$ty>(&a.0, &b, what)?.into_lua_value(lua)
        });
        $methods.add_method("span_until", |_, this, (other, options): (Value, Value)| {
            let other = userdata::<$ty>(&other, "span_until")?;
            let d = difference(&options, "span_until")?;
            this.0
                .until(configure!(<$difference>::new(other.arg()), d))
                .or_raise()
                .map(Span)
        });
        $methods.add_method("span_since", |_, this, (other, options): (Value, Value)| {
            let other = userdata::<$ty>(&other, "span_since")?;
            let d = difference(&options, "span_since")?;
            this.0
                .since(configure!(<$difference>::new(other.arg()), d))
                .or_raise()
                .map(Span)
        });
        $methods.add_method("duration_until", |_, this, other: Value| {
            let other = userdata::<$ty>(&other, "duration_until")?;
            Ok(SignedDuration(this.0.duration_until(other.arg())))
        });
        $methods.add_method("duration_since", |_, this, other: Value| {
            let other = userdata::<$ty>(&other, "duration_since")?;
            Ok(SignedDuration(this.0.duration_since(other.arg())))
        });
        $methods.add_method("strftime", |_, this, format: Value| {
            strftime(&format, this.arg())
        });
    };
}

/// How jiff takes a value as an argument: `Zoned` by reference, and the rest, which are `Copy`,
/// by value.
trait Operand {
    type Arg<'a>
    where
        Self: 'a;
    fn arg(&self) -> Self::Arg<'_>;
}

macro_rules! impl_operand {
    (&$ty:ident, $inner:ty) => {
        impl Operand for $ty {
            type Arg<'a> = &'a $inner;
            fn arg(&self) -> &$inner {
                &self.0
            }
        }
    };
    ($ty:ident, $inner:ty) => {
        impl Operand for $ty {
            type Arg<'a> = $inner;
            fn arg(&self) -> $inner {
                self.0
            }
        }
    };
}

/// A value jiff can add a span or a signed duration to.
trait Arithmetic: Sized {
    type Inner;
    fn add(inner: &Self::Inner, d: Duration) -> std::result::Result<Self, jiff::Error>;
    fn sub(inner: &Self::Inner, d: Duration) -> std::result::Result<Self, jiff::Error>;
}

macro_rules! impl_arithmetic {
    ($ty:ident, $inner:ty) => {
        impl Arithmetic for $ty {
            type Inner = $inner;
            fn add(inner: &$inner, d: Duration) -> std::result::Result<Self, jiff::Error> {
                match d {
                    Duration::Span(s) => inner.checked_add(s),
                    Duration::Signed(s) => inner.checked_add(s),
                }
                .map($ty)
            }
            fn sub(inner: &$inner, d: Duration) -> std::result::Result<Self, jiff::Error> {
                match d {
                    Duration::Span(s) => inner.checked_sub(s),
                    Duration::Signed(s) => inner.checked_sub(s),
                }
                .map($ty)
            }
        }
    };
}

fn add<T: Arithmetic>(inner: &T::Inner, d: &Value, what: &str) -> Result<T> {
    T::add(inner, duration(d, what)?).or_raise()
}

fn sub<T: Arithmetic>(inner: &T::Inner, d: &Value, what: &str) -> Result<T> {
    T::sub(inner, duration(d, what)?).or_raise()
}

trait IntoLuaValue {
    fn into_lua_value(self, lua: &Lua) -> Result<Value>;
}

impl<T: UserData + mlua::MaybeSend + mlua::MaybeSync + 'static> IntoLuaValue for T {
    fn into_lua_value(self, lua: &Lua) -> Result<Value> {
        lua.create_userdata(self).map(Value::UserData)
    }
}

/// The getters jiff gives a date: `year`, `month`, `day`, `weekday` and the rest.
macro_rules! date_getters {
    ($methods:ident) => {
        $methods.add_method("year", |_, this, ()| Ok(this.0.year()));
        $methods.add_method("month", |_, this, ()| Ok(this.0.month()));
        $methods.add_method("day", |_, this, ()| Ok(this.0.day()));
        $methods.add_method("weekday", |_, this, ()| Ok(Weekday(this.0.weekday())));
        $methods.add_method("day_of_year", |_, this, ()| Ok(this.0.day_of_year()));
        $methods.add_method("day_of_year_no_leap", |_, this, ()| {
            Ok(this.0.day_of_year_no_leap())
        });
        $methods.add_method("days_in_month", |_, this, ()| Ok(this.0.days_in_month()));
        $methods.add_method("days_in_year", |_, this, ()| Ok(this.0.days_in_year()));
        $methods.add_method("in_leap_year", |_, this, ()| Ok(this.0.in_leap_year()));
    };
}

/// The date-shaped transformations: `first_of_month`, `tomorrow`, `nth_weekday` and the rest.
/// `$ty` is what each returns; for `Zoned` they can fail, and elsewhere only some can.
macro_rules! date_moves {
    ($methods:ident, $ty:ident, infallible) => {
        $methods.add_method("first_of_month", |_, this, ()| {
            Ok($ty(this.0.first_of_month()))
        });
        $methods.add_method("last_of_month", |_, this, ()| {
            Ok($ty(this.0.last_of_month()))
        });
        $methods.add_method("first_of_year", |_, this, ()| {
            Ok($ty(this.0.first_of_year()))
        });
        $methods.add_method("last_of_year", |_, this, ()| Ok($ty(this.0.last_of_year())));
        date_moves!($methods, $ty);
    };
    ($methods:ident, $ty:ident, fallible) => {
        $methods.add_method("first_of_month", |_, this, ()| {
            this.0.first_of_month().or_raise().map($ty)
        });
        $methods.add_method("last_of_month", |_, this, ()| {
            this.0.last_of_month().or_raise().map($ty)
        });
        $methods.add_method("first_of_year", |_, this, ()| {
            this.0.first_of_year().or_raise().map($ty)
        });
        $methods.add_method("last_of_year", |_, this, ()| {
            this.0.last_of_year().or_raise().map($ty)
        });
        date_moves!($methods, $ty);
    };
    ($methods:ident, $ty:ident) => {
        $methods.add_method("tomorrow", |_, this, ()| {
            this.0.tomorrow().or_raise().map($ty)
        });
        $methods.add_method("yesterday", |_, this, ()| {
            this.0.yesterday().or_raise().map($ty)
        });
        $methods.add_method("nth_weekday", |_, this, (nth, day): (Value, Value)| {
            let nth = narrow(&nth, "nth_weekday")?;
            let day = weekday(&day, "nth_weekday")?;
            this.0.nth_weekday(nth, day).or_raise().map($ty)
        });
        $methods.add_method(
            "nth_weekday_of_month",
            |_, this, (nth, day): (Value, Value)| {
                let nth = narrow(&nth, "nth_weekday_of_month")?;
                let day = weekday(&day, "nth_weekday_of_month")?;
                this.0.nth_weekday_of_month(nth, day).or_raise().map($ty)
            },
        );
    };
}

/// The getters jiff gives a time of day.
macro_rules! time_getters {
    ($methods:ident) => {
        $methods.add_method("hour", |_, this, ()| Ok(this.0.hour()));
        $methods.add_method("minute", |_, this, ()| Ok(this.0.minute()));
        $methods.add_method("second", |_, this, ()| Ok(this.0.second()));
        $methods.add_method("millisecond", |_, this, ()| Ok(this.0.millisecond()));
        $methods.add_method("microsecond", |_, this, ()| Ok(this.0.microsecond()));
        $methods.add_method("nanosecond", |_, this, ()| Ok(this.0.nanosecond()));
        $methods.add_method("subsec_nanosecond", |_, this, ()| {
            Ok(this.0.subsec_nanosecond())
        });
    };
}

/// `round`, for the types whose rounding builder takes a smallest unit, a mode and an increment.
macro_rules! rounding {
    ($methods:ident, $ty:ident, $round:path) => {
        $methods.add_method("round", |_, this, options: Value| {
            let r = round(&options, "round")?;
            this.0
                .round(configure_round!(<$round>::new(), r))
                .or_raise()
                .map($ty)
        });
    };
}

/// What a `with` table's field does to one of jiff's `*With` builders: `Ok` with the builder
/// the field was applied to, or `Err` handing the builder back when the field is not this one's.
type Field<W> = Result<std::result::Result<W, W>>;

/// Applies a `with` table to `builder`, one field at a time, raising on a key `apply` hands back.
fn build_with<W>(
    builder: W,
    fields: &Value,
    keys: &[&str],
    mut apply: impl FnMut(W, &str, &Value) -> Field<W>,
) -> Result<W> {
    let fields = table_arg(fields, "with")?;
    let mut builder = Some(builder);
    each_field(&fields, "with", keys, |key, v| {
        let b = builder
            .take()
            .expect("the builder is put back after every field");
        let (b, found) = match apply(b, key, &v)? {
            Ok(b) => (b, true),
            Err(b) => (b, false),
        };
        builder = Some(b);
        Ok(found)
    })?;
    Ok(builder.expect("the builder is put back after every field"))
}

/// A `with` table's date fields, applied to one of jiff's `*With` builders.
fn date_field<W: DateFields>(w: W, key: &str, v: &Value) -> Field<W> {
    Ok(Ok(match key {
        "year" => w.year(narrow(v, key)?),
        "month" => w.month(narrow(v, key)?),
        "day" => w.day(narrow(v, key)?),
        "day_of_year" => w.day_of_year(narrow(v, key)?),
        "day_of_year_no_leap" => w.day_of_year_no_leap(narrow(v, key)?),
        _ => return Ok(Err(w)),
    }))
}

const DATE_FIELDS: &[&str] = &["year", "month", "day", "day_of_year", "day_of_year_no_leap"];

/// A `with` table's time fields, applied to one of jiff's `*With` builders.
fn time_field<W: TimeFields>(w: W, key: &str, v: &Value) -> Field<W> {
    Ok(Ok(match key {
        "hour" => w.hour(narrow(v, key)?),
        "minute" => w.minute(narrow(v, key)?),
        "second" => w.second(narrow(v, key)?),
        "millisecond" => w.millisecond(narrow(v, key)?),
        "microsecond" => w.microsecond(narrow(v, key)?),
        "nanosecond" => w.nanosecond(narrow(v, key)?),
        "subsec_nanosecond" => w.subsec_nanosecond(narrow(v, key)?),
        _ => return Ok(Err(w)),
    }))
}

const TIME_FIELDS: &[&str] = &[
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
    "subsec_nanosecond",
];

trait DateFields: Sized {
    fn year(self, v: i16) -> Self;
    fn month(self, v: i8) -> Self;
    fn day(self, v: i8) -> Self;
    fn day_of_year(self, v: i16) -> Self;
    fn day_of_year_no_leap(self, v: i16) -> Self;
}

trait TimeFields: Sized {
    fn hour(self, v: i8) -> Self;
    fn minute(self, v: i8) -> Self;
    fn second(self, v: i8) -> Self;
    fn millisecond(self, v: i16) -> Self;
    fn microsecond(self, v: i16) -> Self;
    fn nanosecond(self, v: i16) -> Self;
    fn subsec_nanosecond(self, v: i32) -> Self;
}

macro_rules! impl_fields {
    (date: $($w:ty),*) => {$(
        impl DateFields for $w {
            fn year(self, v: i16) -> Self { <$w>::year(self, v) }
            fn month(self, v: i8) -> Self { <$w>::month(self, v) }
            fn day(self, v: i8) -> Self { <$w>::day(self, v) }
            fn day_of_year(self, v: i16) -> Self { <$w>::day_of_year(self, v) }
            fn day_of_year_no_leap(self, v: i16) -> Self { <$w>::day_of_year_no_leap(self, v) }
        }
    )*};
    (time: $($w:ty),*) => {$(
        impl TimeFields for $w {
            fn hour(self, v: i8) -> Self { <$w>::hour(self, v) }
            fn minute(self, v: i8) -> Self { <$w>::minute(self, v) }
            fn second(self, v: i8) -> Self { <$w>::second(self, v) }
            fn millisecond(self, v: i16) -> Self { <$w>::millisecond(self, v) }
            fn microsecond(self, v: i16) -> Self { <$w>::microsecond(self, v) }
            fn nanosecond(self, v: i16) -> Self { <$w>::nanosecond(self, v) }
            fn subsec_nanosecond(self, v: i32) -> Self { <$w>::subsec_nanosecond(self, v) }
        }
    )*};
}

impl_fields!(date: civil::DateWith, civil::DateTimeWith, jiff::ZonedWith);
impl_fields!(time: civil::TimeWith, civil::DateTimeWith, jiff::ZonedWith);

fn known(groups: &[&[&'static str]]) -> Vec<&'static str> {
    groups.iter().flat_map(|g| g.iter().copied()).collect()
}

// -- Timestamp ---------------------------------------------------------------------------------

/// An instant, with no time zone: jiff's `Timestamp`.
#[derive(Clone, Copy)]
pub struct Timestamp(jiff::Timestamp);

impl_arithmetic!(Timestamp, jiff::Timestamp);
impl_operand!(Timestamp, jiff::Timestamp);

impl UserData for Timestamp {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, Timestamp, ordered);
        arithmetic!(methods, Timestamp, jiff::TimestampDifference);
        rounding!(methods, Timestamp, jiff::TimestampRound);
        methods.add_method("as_second", |_, this, ()| Ok(this.0.as_second()));
        methods.add_method("as_millisecond", |_, this, ()| Ok(this.0.as_millisecond()));
        methods.add_method("as_microsecond", |_, this, ()| Ok(this.0.as_microsecond()));
        methods.add_method("as_nanosecond", |_, this, ()| {
            i128_to_i64(this.0.as_nanosecond(), "as_nanosecond")
        });
        methods.add_method("subsec_millisecond", |_, this, ()| {
            Ok(this.0.subsec_millisecond())
        });
        methods.add_method("subsec_microsecond", |_, this, ()| {
            Ok(this.0.subsec_microsecond())
        });
        methods.add_method("subsec_nanosecond", |_, this, ()| {
            Ok(this.0.subsec_nanosecond())
        });
        methods.add_method("signum", |_, this, ()| Ok(this.0.signum()));
        methods.add_method("is_zero", |_, this, ()| Ok(this.0.is_zero()));
        methods.add_method("as_duration", |_, this, ()| {
            Ok(SignedDuration(this.0.as_duration()))
        });
        methods.add_method("to_zoned", |_, this, zone: Value| {
            let zone = userdata::<TimeZone>(&zone, "to_zoned")?;
            Ok(Zoned(this.0.to_zoned(zone.0)))
        });
        methods.add_method("in_tz", |_, this, name: Value| {
            this.0.in_tz(&string(&name, "in_tz")?).or_raise().map(Zoned)
        });
    }
}

fn timestamp_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "now",
        lua.create_function(|_, ()| Ok(Timestamp(jiff::Timestamp::now())))?,
    )?;
    t.set(
        "new",
        lua.create_function(|_, (second, nanosecond): (Value, Value)| {
            let second = integer(&second, "Timestamp.new")?;
            let nanosecond = narrow_or(&nanosecond, 0, "Timestamp.new")?;
            jiff::Timestamp::new(second, nanosecond)
                .or_raise()
                .map(Timestamp)
        })?,
    )?;
    t.set(
        "from_second",
        lua.create_function(|_, n: Value| {
            jiff::Timestamp::from_second(integer(&n, "from_second")?)
                .or_raise()
                .map(Timestamp)
        })?,
    )?;
    t.set(
        "from_millisecond",
        lua.create_function(|_, n: Value| {
            jiff::Timestamp::from_millisecond(integer(&n, "from_millisecond")?)
                .or_raise()
                .map(Timestamp)
        })?,
    )?;
    t.set(
        "from_microsecond",
        lua.create_function(|_, n: Value| {
            jiff::Timestamp::from_microsecond(integer(&n, "from_microsecond")?)
                .or_raise()
                .map(Timestamp)
        })?,
    )?;
    t.set(
        "from_nanosecond",
        lua.create_function(|_, n: Value| {
            jiff::Timestamp::from_nanosecond(integer(&n, "from_nanosecond")?.into())
                .or_raise()
                .map(Timestamp)
        })?,
    )?;
    t.set(
        "from_duration",
        lua.create_function(|_, d: Value| {
            let d = userdata::<SignedDuration>(&d, "from_duration")?;
            jiff::Timestamp::from_duration(d.0)
                .or_raise()
                .map(Timestamp)
        })?,
    )?;
    t.set(
        "parse",
        lua.create_function(|_, s: Value| parse(&s, "Timestamp.parse").map(Timestamp))?,
    )?;
    t.set(
        "strptime",
        lua.create_function(|_, (format, s): (Value, Value)| {
            let format = string(&format, "Timestamp.strptime")?;
            jiff::Timestamp::strptime(format, string(&s, "Timestamp.strptime")?)
                .or_raise()
                .map(Timestamp)
        })?,
    )?;
    Ok(t)
}

// -- Zoned -------------------------------------------------------------------------------------

/// An instant in a time zone: jiff's `Zoned`.
#[derive(Clone)]
pub struct Zoned(jiff::Zoned);

impl_arithmetic!(Zoned, jiff::Zoned);
impl_operand!(&Zoned, jiff::Zoned);

impl UserData for Zoned {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, Zoned, ordered);
        arithmetic!(methods, Zoned, jiff::ZonedDifference);
        rounding!(methods, Zoned, jiff::ZonedRound);
        date_getters!(methods);
        time_getters!(methods);
        date_moves!(methods, Zoned, fallible);
        methods.add_method("start_of_day", |_, this, ()| {
            this.0.start_of_day().or_raise().map(Zoned)
        });
        methods.add_method("end_of_day", |_, this, ()| {
            this.0.end_of_day().or_raise().map(Zoned)
        });
        methods.add_method("timestamp", |_, this, ()| Ok(Timestamp(this.0.timestamp())));
        methods.add_method("datetime", |_, this, ()| Ok(DateTime(this.0.datetime())));
        methods.add_method("date", |_, this, ()| Ok(Date(this.0.date())));
        methods.add_method("time", |_, this, ()| Ok(Time(this.0.time())));
        methods.add_method("time_zone", |_, this, ()| {
            Ok(TimeZone(this.0.time_zone().clone()))
        });
        methods.add_method("with_time_zone", |_, this, zone: Value| {
            let zone = userdata::<TimeZone>(&zone, "with_time_zone")?;
            Ok(Zoned(this.0.with_time_zone(zone.0)))
        });
        methods.add_method("in_tz", |_, this, name: Value| {
            this.0.in_tz(&string(&name, "in_tz")?).or_raise().map(Zoned)
        });
        methods.add_method("with", |_, this, fields: Value| {
            let keys = known(&[
                DATE_FIELDS,
                TIME_FIELDS,
                &["date", "time", "disambiguation"],
            ]);
            build_with(this.0.with(), &fields, &keys, |b, key, v| {
                Ok(Ok(match key {
                    "date" => b.date(userdata::<Date>(v, key)?.0),
                    "time" => b.time(userdata::<Time>(v, key)?.0),
                    "disambiguation" => b.disambiguation(disambiguation(v, key)?),
                    _ => match date_field(b, key, v)? {
                        Ok(b) => b,
                        Err(b) => return time_field(b, key, v),
                    },
                }))
            })?
            .build()
            .or_raise()
            .map(Zoned)
        });
    }
}

fn zoned_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "now",
        lua.create_function(|_, ()| Ok(Zoned(jiff::Zoned::now())))?,
    )?;
    t.set(
        "new",
        lua.create_function(|_, (ts, zone): (Value, Value)| {
            let ts = userdata::<Timestamp>(&ts, "Zoned.new")?;
            let zone = userdata::<TimeZone>(&zone, "Zoned.new")?;
            Ok(Zoned(jiff::Zoned::new(ts.0, zone.0)))
        })?,
    )?;
    t.set(
        "parse",
        lua.create_function(|_, s: Value| parse(&s, "Zoned.parse").map(Zoned))?,
    )?;
    t.set(
        "strptime",
        lua.create_function(|_, (format, s): (Value, Value)| {
            let format = string(&format, "Zoned.strptime")?;
            jiff::Zoned::strptime(format, string(&s, "Zoned.strptime")?)
                .or_raise()
                .map(Zoned)
        })?,
    )?;
    Ok(t)
}

// -- Date --------------------------------------------------------------------------------------

/// A calendar date, with no time or time zone: jiff's `civil::Date`.
#[derive(Clone, Copy)]
pub struct Date(civil::Date);

impl_arithmetic!(Date, civil::Date);
impl_operand!(Date, civil::Date);

fn new_date((year, month, day): (Value, Value, Value)) -> Result<Date> {
    civil::Date::new(
        narrow(&year, "year")?,
        narrow(&month, "month")?,
        narrow(&day, "day")?,
    )
    .or_raise()
    .map(Date)
}

/// `(hour, minute, second, nanosecond)`, the last two optional, as a `civil::Time`.
fn time_of(
    (hour, minute, second, nanosecond): (Value, Value, Value, Value),
) -> Result<civil::Time> {
    civil::Time::new(
        narrow(&hour, "hour")?,
        narrow(&minute, "minute")?,
        narrow_or(&second, 0, "second")?,
        narrow_or(&nanosecond, 0, "nanosecond")?,
    )
    .or_raise()
}

impl UserData for Date {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, Date, ordered);
        arithmetic!(methods, Date, civil::DateDifference);
        date_getters!(methods);
        date_moves!(methods, Date, infallible);
        methods.add_method("at", |_, this, args| {
            Ok(DateTime(this.0.to_datetime(time_of(args)?)))
        });
        methods.add_method("to_datetime", |_, this, time: Value| {
            Ok(DateTime(
                this.0
                    .to_datetime(userdata::<Time>(&time, "to_datetime")?.0),
            ))
        });
        methods.add_method("to_zoned", |_, this, (zone, options): (Value, Value)| {
            let zone = userdata::<TimeZone>(&zone, "to_zoned")?;
            to_zoned(
                &zone.0,
                this.0.to_datetime(civil::Time::midnight()),
                &options,
                "to_zoned",
            )
            .map(Zoned)
        });
        methods.add_method("in_tz", |_, this, (name, options): (Value, Value)| {
            in_tz(
                &name,
                this.0.to_datetime(civil::Time::midnight()),
                &options,
                "in_tz",
            )
            .map(Zoned)
        });
        methods.add_method("with", |_, this, fields: Value| {
            build_with(this.0.with(), &fields, DATE_FIELDS, date_field)?
                .build()
                .or_raise()
                .map(Date)
        });
    }
}

fn date_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("new", lua.create_function(|_, args| new_date(args))?)?;
    t.set(
        "parse",
        lua.create_function(|_, s: Value| parse(&s, "Date.parse").map(Date))?,
    )?;
    t.set(
        "strptime",
        lua.create_function(|_, (format, s): (Value, Value)| {
            let format = string(&format, "Date.strptime")?;
            civil::Date::strptime(format, string(&s, "Date.strptime")?)
                .or_raise()
                .map(Date)
        })?,
    )?;
    Ok(t)
}

// -- Time --------------------------------------------------------------------------------------

/// A time of day, with no date or time zone: jiff's `civil::Time`.
#[derive(Clone, Copy)]
pub struct Time(civil::Time);

impl_arithmetic!(Time, civil::Time);
impl_operand!(Time, civil::Time);

fn new_time(args: (Value, Value, Value, Value)) -> Result<Time> {
    time_of(args).map(Time)
}

impl UserData for Time {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, Time, ordered);
        arithmetic!(methods, Time, civil::TimeDifference);
        rounding!(methods, Time, civil::TimeRound);
        time_getters!(methods);
        methods.add_method("on", |_, this, args| {
            Ok(DateTime(new_date(args)?.0.to_datetime(this.0)))
        });
        methods.add_method("to_datetime", |_, this, date: Value| {
            Ok(DateTime(
                this.0
                    .to_datetime(userdata::<Date>(&date, "to_datetime")?.0),
            ))
        });
        methods.add_method("with", |_, this, fields: Value| {
            build_with(this.0.with(), &fields, TIME_FIELDS, time_field)?
                .build()
                .or_raise()
                .map(Time)
        });
    }
}

fn time_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("new", lua.create_function(|_, args| new_time(args))?)?;
    t.set(
        "midnight",
        lua.create_function(|_, ()| Ok(Time(civil::Time::midnight())))?,
    )?;
    t.set(
        "parse",
        lua.create_function(|_, s: Value| parse(&s, "Time.parse").map(Time))?,
    )?;
    t.set(
        "strptime",
        lua.create_function(|_, (format, s): (Value, Value)| {
            let format = string(&format, "Time.strptime")?;
            civil::Time::strptime(format, string(&s, "Time.strptime")?)
                .or_raise()
                .map(Time)
        })?,
    )?;
    Ok(t)
}

// -- DateTime ----------------------------------------------------------------------------------

/// A date and a time of day, with no time zone: jiff's `civil::DateTime`.
#[derive(Clone, Copy)]
pub struct DateTime(civil::DateTime);

impl_arithmetic!(DateTime, civil::DateTime);
impl_operand!(DateTime, civil::DateTime);

fn new_datetime(args: mlua::Variadic<Value>) -> Result<DateTime> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Nil);
    if args.len() > 7 {
        return Err(raise(format!(
            "datetime: expected at most 7 arguments, got {}",
            args.len()
        )));
    }
    let date = new_date((arg(0), arg(1), arg(2)))?;
    let hour = if arg(3).is_nil() {
        Value::Integer(0)
    } else {
        arg(3)
    };
    let minute = if arg(4).is_nil() {
        Value::Integer(0)
    } else {
        arg(4)
    };
    let time = time_of((hour, minute, arg(5), arg(6)))?;
    Ok(DateTime(date.0.to_datetime(time)))
}

impl UserData for DateTime {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, DateTime, ordered);
        arithmetic!(methods, DateTime, civil::DateTimeDifference);
        rounding!(methods, DateTime, civil::DateTimeRound);
        date_getters!(methods);
        time_getters!(methods);
        date_moves!(methods, DateTime, infallible);
        methods.add_method("start_of_day", |_, this, ()| {
            Ok(DateTime(this.0.start_of_day()))
        });
        methods.add_method("end_of_day", |_, this, ()| {
            Ok(DateTime(this.0.end_of_day()))
        });
        methods.add_method("date", |_, this, ()| Ok(Date(this.0.date())));
        methods.add_method("time", |_, this, ()| Ok(Time(this.0.time())));
        methods.add_method("to_zoned", |_, this, (zone, options): (Value, Value)| {
            let zone = userdata::<TimeZone>(&zone, "to_zoned")?;
            to_zoned(&zone.0, this.0, &options, "to_zoned").map(Zoned)
        });
        methods.add_method("in_tz", |_, this, (name, options): (Value, Value)| {
            in_tz(&name, this.0, &options, "in_tz").map(Zoned)
        });
        methods.add_method("with", |_, this, fields: Value| {
            let keys = known(&[DATE_FIELDS, TIME_FIELDS, &["date", "time"]]);
            build_with(this.0.with(), &fields, &keys, |b, key, v| {
                Ok(Ok(match key {
                    "date" => b.date(userdata::<Date>(v, key)?.0),
                    "time" => b.time(userdata::<Time>(v, key)?.0),
                    _ => match date_field(b, key, v)? {
                        Ok(b) => b,
                        Err(b) => return time_field(b, key, v),
                    },
                }))
            })?
            .build()
            .or_raise()
            .map(DateTime)
        });
    }
}

fn datetime_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("new", lua.create_function(|_, args| new_datetime(args))?)?;
    t.set(
        "parse",
        lua.create_function(|_, s: Value| parse(&s, "DateTime.parse").map(DateTime))?,
    )?;
    t.set(
        "strptime",
        lua.create_function(|_, (format, s): (Value, Value)| {
            let format = string(&format, "DateTime.strptime")?;
            civil::DateTime::strptime(format, string(&s, "DateTime.strptime")?)
                .or_raise()
                .map(DateTime)
        })?,
    )?;
    Ok(t)
}

// -- Span --------------------------------------------------------------------------------------

/// A span of time in calendar and clock units: jiff's `Span`.
#[derive(Clone, Copy)]
pub struct Span(jiff::Span);

const SPAN_UNITS: &[&str] = &[
    "years",
    "months",
    "weeks",
    "days",
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];

/// Sets one of a span's units, by its plural name, as jiff's `try_*` setters do.
fn set_unit(span: jiff::Span, unit: &str, v: &Value) -> Result<Option<jiff::Span>> {
    let n = || integer(v, unit);
    Ok(Some(
        match unit {
            "years" => span.try_years(n()?),
            "months" => span.try_months(n()?),
            "weeks" => span.try_weeks(n()?),
            "days" => span.try_days(n()?),
            "hours" => span.try_hours(n()?),
            "minutes" => span.try_minutes(n()?),
            "seconds" => span.try_seconds(n()?),
            "milliseconds" => span.try_milliseconds(n()?),
            "microseconds" => span.try_microseconds(n()?),
            "nanoseconds" => span.try_nanoseconds(n()?),
            _ => return Ok(None),
        }
        .or_raise()?,
    ))
}

fn span_with(span: jiff::Span, fields: &Table) -> Result<jiff::Span> {
    let mut span = span;
    each_field(fields, "span", SPAN_UNITS, |key, v| {
        match set_unit(span, key, &v)? {
            Some(s) => span = s,
            None => return Ok(false),
        }
        Ok(true)
    })?;
    Ok(span)
}

/// What a span's calendar units are measured from, for the methods that need one: `relative`, a
/// `Date`, `DateTime` or `Zoned`, or `days_are_24_hours`, which says days have no calendar.
enum Relative {
    None,
    Date(civil::Date),
    DateTime(civil::DateTime),
    Zoned(jiff::Zoned),
    Days24,
}

impl Relative {
    fn from(value: &Value, what: &str) -> Result<Relative> {
        if let Value::UserData(ud) = value {
            if let Ok(d) = ud.borrow::<Date>() {
                return Ok(Relative::Date(d.0));
            }
            if let Ok(d) = ud.borrow::<DateTime>() {
                return Ok(Relative::DateTime(d.0));
            }
            if let Ok(z) = ud.borrow::<Zoned>() {
                return Ok(Relative::Zoned(z.0.clone()));
            }
        }
        Err(raise(format!(
            "{what}: relative must be a Date, DateTime or Zoned, got {}",
            describe(value)
        )))
    }

    fn to(&self) -> Option<jiff::SpanRelativeTo<'_>> {
        match self {
            Relative::Date(d) => Some((*d).into()),
            Relative::DateTime(d) => Some((*d).into()),
            Relative::Zoned(z) => Some(z.into()),
            Relative::None | Relative::Days24 => None,
        }
    }
}

/// A span method's options: `relative` and `days_are_24_hours` always, and, when `rounding`, the
/// options `round` takes as well.
struct SpanOptions {
    relative: Relative,
    difference: Difference,
}

fn span_options(options: &Value, what: &str, rounding: bool) -> Result<SpanOptions> {
    let mut relative = Relative::None;
    let mut days24 = false;
    let mut d = Difference::default();
    if options.is_nil() {
        return Ok(SpanOptions {
            relative,
            difference: d,
        });
    }
    let keys: Vec<&str> = if rounding {
        known(&[&["relative", "days_are_24_hours"], DIFFERENCE_KEYS])
    } else {
        vec!["relative", "days_are_24_hours"]
    };
    each_field(&table_arg(options, what)?, what, &keys, |key, v| {
        match key {
            "relative" => relative = Relative::from(&v, what)?,
            "days_are_24_hours" => match v {
                Value::Boolean(b) => days24 = b,
                other => {
                    return Err(raise(format!(
                        "{what}: days_are_24_hours must be a boolean, got {}",
                        describe(&other)
                    )));
                }
            },
            "largest" if rounding => d.largest = Some(unit(&v, what)?),
            "smallest" if rounding => d.smallest = Some(unit(&v, what)?),
            "mode" if rounding => d.mode = Some(round_mode(&v, what)?),
            "increment" if rounding => d.increment = Some(integer(&v, what)?),
            _ => return Ok(false),
        }
        Ok(true)
    })?;
    if days24 {
        if !matches!(relative, Relative::None) {
            return Err(raise(format!(
                "{what}: give relative or days_are_24_hours, not both"
            )));
        }
        relative = Relative::Days24;
    }
    Ok(SpanOptions {
        relative,
        difference: d,
    })
}

/// `other`, a span or a signed duration, as a span.
fn span_operand(other: &Value, what: &str) -> Result<jiff::Span> {
    match duration(other, what)? {
        Duration::Span(s) => Ok(s),
        Duration::Signed(d) => jiff::Span::try_from(d).or_raise(),
    }
}

fn span_add(
    this: jiff::Span,
    other: &Value,
    options: &Value,
    negate: bool,
    what: &str,
) -> Result<Span> {
    let mut other = span_operand(other, what)?;
    if negate {
        other = other.negate();
    }
    let o = span_options(options, what, false)?;
    let result = match (&o.relative, o.relative.to()) {
        (_, Some(r)) => this.checked_add((other, r)),
        (Relative::Days24, None) => {
            this.checked_add(jiff::SpanArithmetic::from(other).days_are_24_hours())
        }
        _ => this.checked_add(other),
    };
    result.or_raise().map(Span)
}

impl UserData for Span {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.0.to_string()));
        // Fieldwise: `1 hour` and `60 minutes` are different spans, which `compare` says are
        // equal. jiff's `Span` has no `==` of its own for this reason.
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: Value| {
            Ok(match &other {
                Value::UserData(ud) => ud
                    .borrow::<Span>()
                    .is_ok_and(|o| o.0.fieldwise() == this.0.fieldwise()),
                _ => false,
            })
        });
        for (meta, op) in [(MetaMethod::Lt, "<"), (MetaMethod::Le, "<=")] {
            methods.add_meta_function(meta, move |_, (_, _): (Value, Value)| -> Result<bool> {
                Err(raise(format!(
                    "cannot compare spans with {op}: how long a day or month is depends on \
                     when it starts, so use span:compare(other, {{ relative = ... }})"
                )))
            });
        }
        for (meta, op, method) in [(MetaMethod::Add, "+", "add"), (MetaMethod::Sub, "-", "sub")] {
            methods.add_meta_function(meta, move |_, (a, b): (Value, Value)| -> Result<Span> {
                if is::<Span>(&a) && !is::<Span>(&b) && !is::<SignedDuration>(&b) {
                    return Err(raise(format!(
                        "cannot compute Span {op} {}: put the span on the right",
                        describe(&b)
                    )));
                }
                Err(raise(format!(
                    "cannot {method} spans with {op}: how long a day or month is depends on \
                     when it starts, so use span:{method}(other, {{ relative = ... }})"
                )))
            });
        }
        methods.add_meta_method(MetaMethod::Unm, |_, this, ()| Ok(Span(this.0.negate())));
        methods.add_meta_function(MetaMethod::Mul, |_, (a, b): (Value, Value)| {
            let (span, n) = if is::<Span>(&a) { (a, b) } else { (b, a) };
            let span = userdata::<Span>(&span, "Span *")?;
            span.0
                .checked_mul(integer(&n, "Span *")?)
                .or_raise()
                .map(Span)
        });
        macro_rules! units {
            ($($get:ident $set:ident),*) => {$(
                methods.add_method(stringify!($get), |_, this, ()| Ok(this.0.$get()));
                methods.add_method(stringify!($set), |_, this, n: Value| {
                    set_unit(this.0, stringify!($set), &n).map(|s| Span(s.expect("a unit")))
                });
            )*};
        }
        units!(
            get_years years, get_months months, get_weeks weeks, get_days days,
            get_hours hours, get_minutes minutes, get_seconds seconds,
            get_milliseconds milliseconds, get_microseconds microseconds,
            get_nanoseconds nanoseconds
        );
        methods.add_method("abs", |_, this, ()| Ok(Span(this.0.abs())));
        methods.add_method("negate", |_, this, ()| Ok(Span(this.0.negate())));
        methods.add_method("signum", |_, this, ()| Ok(this.0.signum()));
        methods.add_method("is_zero", |_, this, ()| Ok(this.0.is_zero()));
        methods.add_method("is_negative", |_, this, ()| Ok(this.0.is_negative()));
        methods.add_method("is_positive", |_, this, ()| Ok(this.0.is_positive()));
        methods.add_method("mul", |_, this, n: Value| {
            this.0.checked_mul(integer(&n, "mul")?).or_raise().map(Span)
        });
        methods.add_method("add", |_, this, (other, options): (Value, Value)| {
            span_add(this.0, &other, &options, false, "add")
        });
        methods.add_method("sub", |_, this, (other, options): (Value, Value)| {
            span_add(this.0, &other, &options, true, "sub")
        });
        methods.add_method("compare", |_, this, (other, options): (Value, Value)| {
            let other = span_operand(&other, "compare")?;
            let o = span_options(&options, "compare", false)?;
            let ordering = match (&o.relative, o.relative.to()) {
                (_, Some(r)) => this.0.compare((other, r)),
                (Relative::Days24, None) => this
                    .0
                    .compare(jiff::SpanCompare::from(other).days_are_24_hours()),
                _ => this.0.compare(other),
            };
            Ok(ordering.or_raise()? as i8)
        });
        methods.add_method("total", |_, this, (u, options): (Value, Value)| {
            let u = unit(&u, "total")?;
            let o = span_options(&options, "total", false)?;
            match (&o.relative, o.relative.to()) {
                (_, Some(r)) => this.0.total((u, r)),
                (Relative::Days24, None) => {
                    this.0.total(jiff::SpanTotal::from(u).days_are_24_hours())
                }
                _ => this.0.total(u),
            }
            .or_raise()
        });
        methods.add_method("round", |_, this, options: Value| {
            let (u, table) = match &options {
                Value::String(_) => (Some(unit(&options, "round")?), Value::Nil),
                other => (None, other.clone()),
            };
            let o = span_options(&table, "round", true)?;
            let mut r = configure!(jiff::SpanRound::new(), o.difference);
            if let Some(u) = u {
                r = r.smallest(u);
            }
            match (&o.relative, o.relative.to()) {
                (_, Some(rel)) => this.0.round(r.relative(rel)),
                (Relative::Days24, None) => this.0.round(r.days_are_24_hours()),
                _ => this.0.round(r),
            }
            .or_raise()
            .map(Span)
        });
        methods.add_method("to_duration", |_, this, options: Value| {
            let o = span_options(&options, "to_duration", false)?;
            match (&o.relative, o.relative.to()) {
                (_, Some(r)) => this.0.to_duration(r),
                (Relative::Days24, None) => this
                    .0
                    .to_duration(jiff::SpanRelativeTo::days_are_24_hours()),
                _ => jiff::SignedDuration::try_from(this.0),
            }
            .or_raise()
            .map(SignedDuration)
        });
    }
}

fn span_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "new",
        lua.create_function(|_, ()| Ok(Span(jiff::Span::new())))?,
    )?;
    t.set(
        "parse",
        lua.create_function(|_, s: Value| parse(&s, "Span.parse").map(Span))?,
    )?;
    Ok(t)
}

// -- SignedDuration ----------------------------------------------------------------------------

/// An exact length of time, in seconds and nanoseconds: jiff's `SignedDuration`.
#[derive(Clone, Copy)]
pub struct SignedDuration(jiff::SignedDuration);

fn duration_pair(
    a: &Value,
    b: &Value,
    op: &str,
) -> Result<(jiff::SignedDuration, jiff::SignedDuration)> {
    let what = format!("SignedDuration {op}");
    Ok((
        userdata::<SignedDuration>(a, &what)?.0,
        userdata::<SignedDuration>(b, &what)?.0,
    ))
}

fn overflow(what: &str) -> mlua::Error {
    raise(format!("{what}: the result overflows a SignedDuration"))
}

/// A SignedDuration and an integer, in either order: what `*` is given.
fn scaled(a: &Value, b: &Value, what: &str) -> Result<(jiff::SignedDuration, i32)> {
    let (d, n) = if is::<SignedDuration>(a) {
        (a, b)
    } else {
        (b, a)
    };
    Ok((userdata::<SignedDuration>(d, what)?.0, narrow(n, what)?))
}

impl UserData for SignedDuration {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, SignedDuration, ordered);
        methods.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            let (a, b) = duration_pair(&a, &b, "+")?;
            a.checked_add(b)
                .map(SignedDuration)
                .ok_or_else(|| overflow("+"))
        });
        methods.add_meta_function(MetaMethod::Sub, |_, (a, b): (Value, Value)| {
            let (a, b) = duration_pair(&a, &b, "-")?;
            a.checked_sub(b)
                .map(SignedDuration)
                .ok_or_else(|| overflow("-"))
        });
        methods.add_meta_method(MetaMethod::Unm, |_, this, ()| {
            this.0
                .checked_neg()
                .map(SignedDuration)
                .ok_or_else(|| overflow("-"))
        });
        methods.add_meta_function(MetaMethod::Mul, |_, (a, b): (Value, Value)| {
            let (d, n) = scaled(&a, &b, "SignedDuration *")?;
            d.checked_mul(n)
                .map(SignedDuration)
                .ok_or_else(|| overflow("*"))
        });
        methods.add_meta_function(MetaMethod::Div, |_, (a, b): (Value, Value)| {
            let d = userdata::<SignedDuration>(&a, "SignedDuration /")?.0;
            let n: i32 = narrow(&b, "SignedDuration /")?;
            if n == 0 {
                return Err(raise("SignedDuration /: division by zero"));
            }
            d.checked_div(n)
                .map(SignedDuration)
                .ok_or_else(|| overflow("/"))
        });
        methods.add_method("add", |_, this, other: Value| {
            let other = userdata::<SignedDuration>(&other, "add")?.0;
            this.0
                .checked_add(other)
                .map(SignedDuration)
                .ok_or_else(|| overflow("add"))
        });
        methods.add_method("sub", |_, this, other: Value| {
            let other = userdata::<SignedDuration>(&other, "sub")?.0;
            this.0
                .checked_sub(other)
                .map(SignedDuration)
                .ok_or_else(|| overflow("sub"))
        });
        methods.add_method("mul", |_, this, n: Value| {
            let n = narrow(&n, "mul")?;
            this.0
                .checked_mul(n)
                .map(SignedDuration)
                .ok_or_else(|| overflow("mul"))
        });
        methods.add_method("div", |_, this, n: Value| {
            let n: i32 = narrow(&n, "div")?;
            if n == 0 {
                return Err(raise("div: division by zero"));
            }
            this.0
                .checked_div(n)
                .map(SignedDuration)
                .ok_or_else(|| overflow("div"))
        });
        methods.add_method("neg", |_, this, ()| {
            this.0
                .checked_neg()
                .map(SignedDuration)
                .ok_or_else(|| overflow("neg"))
        });
        methods.add_method("abs", |_, this, ()| {
            if this.0 == jiff::SignedDuration::MIN {
                return Err(overflow("abs"));
            }
            Ok(SignedDuration(this.0.abs()))
        });
        methods.add_method("as_secs", |_, this, ()| Ok(this.0.as_secs()));
        methods.add_method("as_mins", |_, this, ()| Ok(this.0.as_mins()));
        methods.add_method("as_hours", |_, this, ()| Ok(this.0.as_hours()));
        methods.add_method("as_millis", |_, this, ()| {
            i128_to_i64(this.0.as_millis(), "as_millis")
        });
        methods.add_method("as_micros", |_, this, ()| {
            i128_to_i64(this.0.as_micros(), "as_micros")
        });
        methods.add_method("as_nanos", |_, this, ()| {
            i128_to_i64(this.0.as_nanos(), "as_nanos")
        });
        methods.add_method("as_secs_f64", |_, this, ()| Ok(this.0.as_secs_f64()));
        methods.add_method("as_millis_f64", |_, this, ()| Ok(this.0.as_millis_f64()));
        methods.add_method("subsec_millis", |_, this, ()| Ok(this.0.subsec_millis()));
        methods.add_method("subsec_micros", |_, this, ()| Ok(this.0.subsec_micros()));
        methods.add_method("subsec_nanos", |_, this, ()| Ok(this.0.subsec_nanos()));
        methods.add_method("is_zero", |_, this, ()| Ok(this.0.is_zero()));
        methods.add_method("is_negative", |_, this, ()| Ok(this.0.is_negative()));
        methods.add_method("is_positive", |_, this, ()| Ok(this.0.is_positive()));
        methods.add_method("signum", |_, this, ()| Ok(this.0.signum()));
        rounding!(methods, SignedDuration, jiff::SignedDurationRound);
    }
}

fn signed_duration_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "new",
        lua.create_function(|_, (secs, nanos): (Value, Value)| {
            let secs = integer(&secs, "SignedDuration.new")?;
            let nanos: i32 = narrow_or(&nanos, 0, "SignedDuration.new")?;
            // jiff's `new` panics where the nanoseconds carry the seconds past `i64`.
            let carry = i64::from(nanos / 1_000_000_000);
            secs.checked_add(carry)
                .map(|_| SignedDuration(jiff::SignedDuration::new(secs, nanos)))
                .ok_or_else(|| overflow("SignedDuration.new"))
        })?,
    )?;
    t.set(
        "from_secs",
        lua.create_function(|_, n: Value| {
            Ok(SignedDuration(jiff::SignedDuration::from_secs(integer(
                &n,
                "from_secs",
            )?)))
        })?,
    )?;
    t.set(
        "from_millis",
        lua.create_function(|_, n: Value| {
            Ok(SignedDuration(jiff::SignedDuration::from_millis(integer(
                &n,
                "from_millis",
            )?)))
        })?,
    )?;
    t.set(
        "from_micros",
        lua.create_function(|_, n: Value| {
            Ok(SignedDuration(jiff::SignedDuration::from_micros(integer(
                &n,
                "from_micros",
            )?)))
        })?,
    )?;
    t.set(
        "from_nanos",
        lua.create_function(|_, n: Value| {
            Ok(SignedDuration(jiff::SignedDuration::from_nanos(integer(
                &n,
                "from_nanos",
            )?)))
        })?,
    )?;
    t.set(
        "from_mins",
        lua.create_function(|_, n: Value| {
            jiff::SignedDuration::try_from_mins(integer(&n, "from_mins")?)
                .ok_or_else(|| overflow("from_mins"))
                .map(SignedDuration)
        })?,
    )?;
    t.set(
        "from_hours",
        lua.create_function(|_, n: Value| {
            jiff::SignedDuration::try_from_hours(integer(&n, "from_hours")?)
                .ok_or_else(|| overflow("from_hours"))
                .map(SignedDuration)
        })?,
    )?;
    t.set(
        "from_secs_f64",
        lua.create_function(|_, n: Value| {
            jiff::SignedDuration::try_from_secs_f64(float(&n, "from_secs_f64")?)
                .or_raise()
                .map(SignedDuration)
        })?,
    )?;
    t.set(
        "parse",
        lua.create_function(|_, s: Value| parse(&s, "SignedDuration.parse").map(SignedDuration))?,
    )?;
    t.set("ZERO", SignedDuration(jiff::SignedDuration::ZERO))?;
    Ok(t)
}

// -- TimeZone ----------------------------------------------------------------------------------

/// A set of rules for turning an instant into a civil time: jiff's `TimeZone`.
#[derive(Clone)]
pub struct TimeZone(jiff::tz::TimeZone);

impl UserData for TimeZone {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // The IANA name where there is one. A zone without one is a fixed offset, or a POSIX
        // rule or zoneinfo file with no name, and says which.
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(match this.0.iana_name() {
                Some(name) => name.to_owned(),
                None => match this.0.to_fixed_offset() {
                    Ok(offset) => offset.to_string(),
                    Err(_) => "TimeZone(unnamed)".to_owned(),
                },
            })
        });
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: Value| {
            Ok(match &other {
                Value::UserData(ud) => ud.borrow::<TimeZone>().is_ok_and(|o| o.0 == this.0),
                _ => false,
            })
        });
        methods.add_method("iana_name", |_, this, ()| {
            Ok(this.0.iana_name().map(str::to_owned))
        });
        methods.add_method("to_zoned", |_, this, (dt, options): (Value, Value)| {
            let dt = userdata::<DateTime>(&dt, "to_zoned")?;
            to_zoned(&this.0, dt.0, &options, "to_zoned").map(Zoned)
        });
        methods.add_method("to_datetime", |_, this, ts: Value| {
            let ts = userdata::<Timestamp>(&ts, "to_datetime")?;
            Ok(DateTime(this.0.to_datetime(ts.0)))
        });
    }
}

fn time_zone_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "get",
        lua.create_function(|_, name: Value| {
            tz::TimeZone::get(&string(&name, "TimeZone.get")?)
                .or_raise()
                .map(TimeZone)
        })?,
    )?;
    t.set(
        "system",
        lua.create_function(|_, ()| Ok(TimeZone(tz::TimeZone::system())))?,
    )?;
    t.set(
        "try_system",
        lua.create_function(|_, ()| tz::TimeZone::try_system().or_raise().map(TimeZone))?,
    )?;
    t.set("UTC", TimeZone(tz::TimeZone::UTC))?;
    Ok(t)
}

// -- Weekday -----------------------------------------------------------------------------------

/// A day of the week: jiff's `civil::Weekday`. A type and not a number, because a number does
/// not say whether the week starts on Sunday or Monday, or at 0 or 1.
#[derive(Clone, Copy)]
pub struct Weekday(civil::Weekday);

const WEEKDAYS: [(civil::Weekday, &str); 7] = [
    (civil::Weekday::Monday, "Monday"),
    (civil::Weekday::Tuesday, "Tuesday"),
    (civil::Weekday::Wednesday, "Wednesday"),
    (civil::Weekday::Thursday, "Thursday"),
    (civil::Weekday::Friday, "Friday"),
    (civil::Weekday::Saturday, "Saturday"),
    (civil::Weekday::Sunday, "Sunday"),
];

fn weekday_name(day: civil::Weekday) -> &'static str {
    WEEKDAYS
        .iter()
        .find(|(d, _)| *d == day)
        .map(|(_, name)| *name)
        .expect("every weekday is in WEEKDAYS")
}

/// A weekday by its English name, full or its first three letters, in any case.
fn weekday_from_name(name: &str, what: &str) -> Result<civil::Weekday> {
    let lower = name.to_ascii_lowercase();
    WEEKDAYS
        .iter()
        .find(|(_, full)| {
            let full = full.to_ascii_lowercase();
            lower == full || lower == full[..3]
        })
        .map(|(day, _)| *day)
        .ok_or_else(|| {
            raise(format!(
                "{what}: \"{name}\" is not a weekday (expected a name such as \"Monday\" or \
                 \"mon\")"
            ))
        })
}

impl UserData for Weekday {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(weekday_name(this.0)));
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: Value| {
            Ok(match &other {
                Value::UserData(ud) => ud.borrow::<Weekday>().is_ok_and(|o| o.0 == this.0),
                _ => false,
            })
        });
        methods.add_method("to_monday_zero_offset", |_, this, ()| {
            Ok(this.0.to_monday_zero_offset())
        });
        methods.add_method("to_monday_one_offset", |_, this, ()| {
            Ok(this.0.to_monday_one_offset())
        });
        methods.add_method("to_sunday_zero_offset", |_, this, ()| {
            Ok(this.0.to_sunday_zero_offset())
        });
        methods.add_method("to_sunday_one_offset", |_, this, ()| {
            Ok(this.0.to_sunday_one_offset())
        });
        methods.add_method("next", |_, this, ()| Ok(Weekday(this.0.next())));
        methods.add_method("previous", |_, this, ()| Ok(Weekday(this.0.previous())));
        // jiff's `until` and `since`: `until` is a Lua keyword, so `w:until(x)` would not parse.
        methods.add_method("days_until", |_, this, other: Value| {
            Ok(this.0.until(weekday(&other, "days_until")?))
        });
        methods.add_method("days_since", |_, this, other: Value| {
            Ok(this.0.since(weekday(&other, "days_since")?))
        });
        methods.add_method("wrapping_add", |_, this, n: Value| {
            Ok(Weekday(this.0.wrapping_add(integer(&n, "wrapping_add")?)))
        });
        methods.add_method("wrapping_sub", |_, this, n: Value| {
            Ok(Weekday(this.0.wrapping_sub(integer(&n, "wrapping_sub")?)))
        });
    }
}

fn weekday_table(lua: &Lua) -> Result<Table> {
    let t = lua.create_table()?;
    for (day, name) in WEEKDAYS {
        t.set(name.to_ascii_lowercase(), Weekday(day))?;
    }
    t.set(
        "from_name",
        lua.create_function(|_, name: Value| {
            weekday_from_name(&string(&name, "Weekday.from_name")?, "Weekday.from_name")
                .map(Weekday)
        })?,
    )?;
    macro_rules! offsets {
        ($($from:ident),*) => {$(
            t.set(
                stringify!($from),
                lua.create_function(|_, n: Value| {
                    civil::Weekday::$from(narrow(&n, stringify!($from))?)
                        .or_raise()
                        .map(Weekday)
                })?,
            )?;
        )*};
    }
    offsets!(
        from_monday_zero_offset,
        from_monday_one_offset,
        from_sunday_zero_offset,
        from_sunday_one_offset
    );
    Ok(t)
}

// -- sleep -------------------------------------------------------------------------------------

/// What `sleep` waits for: a SignedDuration, a Span with no calendar units, or a whole number of
/// milliseconds. A negative length raises rather than returning at once, since it is a mistake.
fn sleep_duration(amount: &Value) -> Result<std::time::Duration> {
    let what = "sleep";
    let signed = match amount {
        Value::Integer(_) | Value::Number(_) => {
            jiff::SignedDuration::from_millis(integer(amount, what)?)
        }
        _ => match duration(amount, what).map_err(|_| {
            raise(format!(
                "{what}: expected a SignedDuration, a Span or a number of milliseconds, got {}",
                describe(amount)
            ))
        })? {
            Duration::Signed(d) => d,
            Duration::Span(s) => jiff::SignedDuration::try_from(s).map_err(|_| {
                raise(format!(
                    "{what}: a span with years, months, weeks or days has no fixed length; \
                     give one in hours or smaller, or a SignedDuration"
                ))
            })?,
        },
    };
    std::time::Duration::try_from(signed).map_err(|_| {
        raise(format!(
            "{what}: cannot sleep for a negative time ({signed})"
        ))
    })
}
