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
//! an `f64`. Arguments past the last one a function takes raise too, rather than being dropped as
//! Lua would drop them: `dt.date(2024, 1, 2, 9, 30)` is a mistake, not a date.
//!
//! Nothing here sets a global. [`rust_half`] builds a table of the types' constructors, and
//! `modules.rs` hands it to `lua/datetime.lua`, which returns it as the module.

use std::fmt::Display;

use jiff::civil;
use jiff::tz::{self, Disambiguation};
use jiff::{RoundMode, Unit};
use mlua::{
    IntoLuaMulti, Lua, MaybeSend, MetaMethod, MultiValue, Table, UserData, UserDataMethods, Value,
};

type Result<T> = mlua::Result<T>;

/// Builds the table `lua/datetime.lua` returns: one table of constructors per type, the
/// shorthands, and `sleep`.
pub fn rust_half(lua: &Lua) -> Result<Value> {
    let module = Constructors::new(lua, "datetime")?;
    module.set("Timestamp", timestamp_table(lua)?)?;
    module.set("Zoned", zoned_table(lua)?)?;
    module.set("Date", date_table(lua)?)?;
    module.set("Time", time_table(lua)?)?;
    module.set("DateTime", datetime_table(lua)?)?;
    module.set("Span", span_table(lua)?)?;
    module.set("SignedDuration", signed_duration_table(lua)?)?;
    module.set("TimeZone", time_zone_table(lua)?)?;
    module.set("Weekday", weekday_table(lua)?)?;
    module.add("date", |what, args| new_date(args, what))?;
    module.add("time", |what, args| new_time(args, what))?;
    module.add("datetime", |what, args| new_datetime(args, what))?;
    module.add("span", |what, fields: Value| {
        span_with(jiff::Span::new(), &table_arg(&fields, what)?, what).map(Span)
    })?;
    module.set(
        "sleep",
        lua.create_async_function(|_, args: MultiValue| {
            let what = "datetime.sleep";
            let amount = take::<Value>(args, what).and_then(|v| sleep_duration(&v, what));
            async move {
                tokio::time::sleep(amount?).await;
                Ok(())
            }
        })?,
    )?;
    Ok(Value::Table(module.table))
}

// -- Registering functions ---------------------------------------------------------------------

/// The arguments a function takes: `()`, one `Value`, or a tuple of them. Lua drops arguments
/// past the last one a function names; these raise instead, since an argument the function will
/// not look at is one the script meant something by.
trait Args: Sized {
    const MAX: usize;
    fn from_values(values: &mut MultiValue) -> Self;
}

impl Args for () {
    const MAX: usize = 0;
    fn from_values(_: &mut MultiValue) -> Self {}
}

impl Args for Value {
    const MAX: usize = 1;
    fn from_values(values: &mut MultiValue) -> Self {
        values.pop_front().unwrap_or(Value::Nil)
    }
}

macro_rules! impl_args {
    ($max:literal: $($v:ident),*) => {
        impl Args for ($($v,)*) {
            const MAX: usize = $max;
            fn from_values(values: &mut MultiValue) -> Self {
                ($(<$v as Args>::from_values(values),)*)
            }
        }
    };
}

impl_args!(2: Value, Value);
impl_args!(3: Value, Value, Value);
impl_args!(4: Value, Value, Value, Value);
impl_args!(7: Value, Value, Value, Value, Value, Value, Value);

fn take<A: Args>(mut values: MultiValue, what: &str) -> Result<A> {
    if values.len() > A::MAX {
        let expected = match A::MAX {
            0 => "no arguments".to_owned(),
            1 => "at most 1 argument".to_owned(),
            n => format!("at most {n} arguments"),
        };
        return Err(raise(format!(
            "{what}: expected {expected}, got {}",
            values.len()
        )));
    }
    Ok(A::from_values(&mut values))
}

/// Registers a method on `T`. The function is given its name as a script would write it,
/// `Date:with`, to begin its error messages with.
fn method<T, M, A, R>(
    methods: &mut M,
    name: &'static str,
    f: impl Fn(&str, &T, A) -> Result<R> + MaybeSend + 'static,
) where
    T: UserData + 'static,
    M: UserDataMethods<T>,
    A: Args,
    R: IntoLuaMulti,
{
    let what = format!("{}:{name}", short_name::<T>());
    methods.add_method(name, move |_, this, args: MultiValue| {
        f(&what, this, take(args, &what)?)
    });
}

/// A table of functions on the module, `Date.parse` and the rest. Each is given its name as a
/// script would write it, `Date.parse`, to begin its error messages with.
struct Constructors<'a> {
    lua: &'a Lua,
    prefix: &'static str,
    table: Table,
}

impl<'a> Constructors<'a> {
    fn new(lua: &'a Lua, prefix: &'static str) -> Result<Self> {
        Ok(Constructors {
            lua,
            prefix,
            table: lua.create_table()?,
        })
    }

    fn add<A: Args, R: IntoLuaMulti>(
        &self,
        name: &'static str,
        f: impl Fn(&str, A) -> Result<R> + MaybeSend + 'static,
    ) -> Result<()> {
        let what = format!("{}.{name}", self.prefix);
        let function = self
            .lua
            .create_function(move |_, args: MultiValue| f(&what, take(args, &what)?))?;
        self.table.set(name, function)
    }

    fn set(&self, name: impl mlua::IntoLua, value: impl mlua::IntoLua) -> Result<()> {
        self.table.set(name, value)
    }

    /// `parse` and `strptime`, which every type with a jiff `FromStr` and `strptime` has.
    fn parsers<
        T: std::str::FromStr<Err = jiff::Error> + 'static,
        W: UserData + MaybeSend + mlua::MaybeSync + 'static,
    >(
        &self,
        wrap: fn(T) -> W,
        strptime: fn(&str, &str) -> std::result::Result<T, jiff::Error>,
    ) -> Result<()> {
        self.add("parse", move |what, s: Value| {
            string(&s, what)?.parse().or_raise().map(wrap)
        })?;
        self.add("strptime", move |what, (format, s): (Value, Value)| {
            let format = string(&format, what)?;
            strptime(&format, &string(&s, what)?).or_raise().map(wrap)
        })
    }

    fn done(self) -> Result<Table> {
        Ok(self.table)
    }
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

/// `value` as an integer that fits `T`: jiff's fields are `i8`, `i16` and `i32`. `field` names
/// the argument, when there are several.
fn narrow<T: TryFrom<i64>>(value: &Value, what: &str, field: &str) -> Result<T> {
    let label = if field.is_empty() {
        what.to_owned()
    } else {
        format!("{what}: {field}")
    };
    let n = integer(value, &label)?;
    T::try_from(n).map_err(|_| raise(format!("{label}: {n} is out of range")))
}

/// Like [`narrow`], but `nil` is `default`: for trailing arguments that may be left off.
fn narrow_or<T: TryFrom<i64>>(value: &Value, default: T, what: &str, field: &str) -> Result<T> {
    match value {
        Value::Nil => Ok(default),
        value => narrow(value, what, field),
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

fn known<'a>(groups: &[&[&'a str]]) -> Vec<&'a str> {
    groups.iter().flat_map(|g| g.iter().copied()).collect()
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

/// What can be added to or subtracted from a value.
enum SpanOrDuration {
    Span(jiff::Span),
    Duration(jiff::SignedDuration),
}

fn span_or_duration(value: &Value, what: &str) -> Result<SpanOrDuration> {
    if let Value::UserData(ud) = value {
        if let Ok(span) = ud.borrow::<Span>() {
            return Ok(SpanOrDuration::Span(span.0));
        }
        if let Ok(duration) = ud.borrow::<SignedDuration>() {
            return Ok(SpanOrDuration::Duration(duration.0));
        }
    }
    Err(raise(format!(
        "{what}: expected a Span or a SignedDuration, got {}",
        describe(value)
    )))
}

// -- Options -----------------------------------------------------------------------------------

/// The options jiff's difference and rounding builders take, as a table. `span_until` and
/// `span_since` take all four; `round` takes all but `largest`, except on a Span.
#[derive(Default)]
struct Rounding {
    largest: Option<Unit>,
    smallest: Option<Unit>,
    mode: Option<RoundMode>,
    increment: Option<i64>,
}

const DIFFERENCE_KEYS: &[&str] = &["largest", "smallest", "mode", "increment"];
const ROUND_KEYS: &[&str] = &["smallest", "mode", "increment"];

impl Rounding {
    /// Takes `key` if it is one of `keys`, and says whether it did.
    fn field(&mut self, keys: &[&str], key: &str, v: &Value, what: &str) -> Result<bool> {
        if !keys.contains(&key) {
            return Ok(false);
        }
        match key {
            "largest" => self.largest = Some(unit(v, what)?),
            "smallest" => self.smallest = Some(unit(v, what)?),
            "mode" => self.mode = Some(round_mode(v, what)?),
            "increment" => self.increment = Some(integer(v, what)?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// An options table of `keys`, or `nil` for none. Where `unit_alone`, a unit string on its
    /// own is `{ smallest = unit }`, as jiff's `round(Unit)` is.
    fn from(options: &Value, keys: &[&str], unit_alone: bool, what: &str) -> Result<Rounding> {
        let mut r = Rounding::default();
        match options {
            Value::Nil => {}
            Value::String(_) if unit_alone => r.smallest = Some(unit(options, what)?),
            _ => each_field(&table_arg(options, what)?, what, keys, |key, v| {
                r.field(keys, key, &v, what)
            })?,
        }
        Ok(r)
    }
}

/// Applies a [`Rounding`] to one of jiff's difference or rounding builders. `largest` is only
/// for the builders that have one.
macro_rules! configure {
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
    ($builder:expr, $r:expr, largest) => {{
        let mut b = configure!($builder, $r);
        if let Some(u) = $r.largest {
            b = b.largest(u);
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
    zone: &tz::TimeZone,
    dt: civil::DateTime,
    options: &Value,
    what: &str,
) -> Result<jiff::Zoned> {
    let d = disambiguation_option(options, what)?;
    zone.to_ambiguous_zoned(dt).disambiguate(d).or_raise()
}

fn in_tz(name: &Value, dt: civil::DateTime, options: &Value, what: &str) -> Result<jiff::Zoned> {
    let zone = tz::TimeZone::get(&string(name, what)?).or_raise()?;
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

/// jiff's strftime, which fails on a bad directive rather than panicking as its `Display` would.
fn strftime(
    format: &Value,
    time: impl Into<jiff::fmt::strtime::BrokenDownTime>,
    what: &str,
) -> Result<String> {
    jiff::fmt::strtime::format(string(format, what)?, time).or_raise()
}

// -- Shared methods ----------------------------------------------------------------------------

/// `__tostring` as jiff's `Display`, `==`, and, when `ordered`, `<` and `<=` within the type.
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

/// How jiff takes a value as an argument: `Zoned` by reference, and the rest, which are `Copy`,
/// by value.
trait Operand {
    type Arg<'a>
    where
        Self: 'a;
    fn arg(&self) -> Self::Arg<'_>;
}

/// A value jiff can add a span or a signed duration to.
trait Arithmetic: Sized {
    type Inner;
    fn add(inner: &Self::Inner, d: SpanOrDuration) -> std::result::Result<Self, jiff::Error>;
    fn sub(inner: &Self::Inner, d: SpanOrDuration) -> std::result::Result<Self, jiff::Error>;
}

/// Wraps a jiff type: the newtype, how jiff takes it as an argument, and its `add` and `sub`.
macro_rules! wrapper {
    ($(#[$doc:meta])* $ty:ident($inner:ty), $by:tt) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $ty($inner);

        impl Arithmetic for $ty {
            type Inner = $inner;
            fn add(inner: &$inner, d: SpanOrDuration) -> std::result::Result<Self, jiff::Error> {
                match d {
                    SpanOrDuration::Span(s) => inner.checked_add(s),
                    SpanOrDuration::Duration(s) => inner.checked_add(s),
                }
                .map($ty)
            }
            fn sub(inner: &$inner, d: SpanOrDuration) -> std::result::Result<Self, jiff::Error> {
                match d {
                    SpanOrDuration::Span(s) => inner.checked_sub(s),
                    SpanOrDuration::Duration(s) => inner.checked_sub(s),
                }
                .map($ty)
            }
        }

        wrapper!(@operand $ty, $inner, $by);
    };
    (@operand $ty:ident, $inner:ty, ref) => {
        impl Operand for $ty {
            type Arg<'a> = &'a $inner;
            fn arg(&self) -> &$inner {
                &self.0
            }
        }
    };
    (@operand $ty:ident, $inner:ty, copy) => {
        impl Operand for $ty {
            type Arg<'a> = $inner;
            fn arg(&self) -> $inner {
                self.0
            }
        }
    };
}

fn add<T: Arithmetic>(inner: &T::Inner, d: &Value, what: &str) -> Result<T> {
    T::add(inner, span_or_duration(d, what)?).or_raise()
}

fn sub<T: Arithmetic>(inner: &T::Inner, d: &Value, what: &str) -> Result<T> {
    T::sub(inner, span_or_duration(d, what)?).or_raise()
}

/// `add`, `sub`, `span_until`, `span_since`, `duration_until`, `duration_since`, `strftime`, and
/// the `+` and `-` operators: `value ± span` is a value, and `value - value` is a span.
///
/// jiff's `until` and `since` are `span_until` and `span_since` here, because `until` is a Lua
/// keyword and `a:until(b)` does not parse. The names pair with `duration_until` and
/// `duration_since`, which give a SignedDuration where these give a Span.
macro_rules! arithmetic {
    ($methods:ident, $ty:ident, $difference:path) => {
        method($methods, "add", |what, this: &$ty, d: Value| {
            add::<$ty>(&this.0, &d, what)
        });
        method($methods, "sub", |what, this: &$ty, d: Value| {
            sub::<$ty>(&this.0, &d, what)
        });
        $methods.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            let what = concat!(stringify!($ty), " +");
            add::<$ty>(&userdata::<$ty>(&a, what)?.0, &b, what)
        });
        $methods.add_meta_function(MetaMethod::Sub, |lua, (a, b): (Value, Value)| {
            let what = concat!(stringify!($ty), " -");
            let a = userdata::<$ty>(&a, what)?;
            if is::<$ty>(&b) {
                let b = userdata::<$ty>(&b, what)?;
                let span = Span(a.0.since(b.arg()).or_raise()?);
                return lua.create_userdata(span).map(Value::UserData);
            }
            lua.create_userdata(sub::<$ty>(&a.0, &b, what)?)
                .map(Value::UserData)
        });
        method(
            $methods,
            "span_until",
            |what, this: &$ty, (other, options): (Value, Value)| {
                let other = userdata::<$ty>(&other, what)?;
                let r = Rounding::from(&options, DIFFERENCE_KEYS, false, what)?;
                this.0
                    .until(configure!(<$difference>::new(other.arg()), r, largest))
                    .or_raise()
                    .map(Span)
            },
        );
        method(
            $methods,
            "span_since",
            |what, this: &$ty, (other, options): (Value, Value)| {
                let other = userdata::<$ty>(&other, what)?;
                let r = Rounding::from(&options, DIFFERENCE_KEYS, false, what)?;
                this.0
                    .since(configure!(<$difference>::new(other.arg()), r, largest))
                    .or_raise()
                    .map(Span)
            },
        );
        method(
            $methods,
            "duration_until",
            |what, this: &$ty, other: Value| {
                let other = userdata::<$ty>(&other, what)?;
                Ok(SignedDuration(this.0.duration_until(other.arg())))
            },
        );
        method(
            $methods,
            "duration_since",
            |what, this: &$ty, other: Value| {
                let other = userdata::<$ty>(&other, what)?;
                Ok(SignedDuration(this.0.duration_since(other.arg())))
            },
        );
        method($methods, "strftime", |what, this: &$ty, format: Value| {
            strftime(&format, this.arg(), what)
        });
    };
}

/// `round`, for the types whose rounding builder takes a smallest unit, a mode and an increment.
macro_rules! rounding {
    ($methods:ident, $ty:ident, $round:path) => {
        method($methods, "round", |what, this: &$ty, options: Value| {
            let r = Rounding::from(&options, ROUND_KEYS, true, what)?;
            this.0
                .round(configure!(<$round>::new(), r))
                .or_raise()
                .map($ty)
        });
    };
}

/// The getters jiff gives a date: `year`, `month`, `day`, `weekday` and the rest.
macro_rules! date_getters {
    ($methods:ident, $ty:ident) => {
        method($methods, "year", |_, this: &$ty, ()| Ok(this.0.year()));
        method($methods, "month", |_, this: &$ty, ()| Ok(this.0.month()));
        method($methods, "day", |_, this: &$ty, ()| Ok(this.0.day()));
        method($methods, "weekday", |_, this: &$ty, ()| {
            Ok(Weekday(this.0.weekday()))
        });
        method($methods, "day_of_year", |_, this: &$ty, ()| {
            Ok(this.0.day_of_year())
        });
        method($methods, "day_of_year_no_leap", |_, this: &$ty, ()| {
            Ok(this.0.day_of_year_no_leap())
        });
        method($methods, "days_in_month", |_, this: &$ty, ()| {
            Ok(this.0.days_in_month())
        });
        method($methods, "days_in_year", |_, this: &$ty, ()| {
            Ok(this.0.days_in_year())
        });
        method($methods, "in_leap_year", |_, this: &$ty, ()| {
            Ok(this.0.in_leap_year())
        });
    };
}

/// The date-shaped moves: `first_of_month`, `tomorrow`, `nth_weekday` and the rest. On a `Zoned`
/// the first four can fail, since the day they land on may start in a DST gap.
macro_rules! date_moves {
    ($methods:ident, $ty:ident, infallible) => {
        method($methods, "first_of_month", |_, this: &$ty, ()| {
            Ok($ty(this.0.first_of_month()))
        });
        method($methods, "last_of_month", |_, this: &$ty, ()| {
            Ok($ty(this.0.last_of_month()))
        });
        method($methods, "first_of_year", |_, this: &$ty, ()| {
            Ok($ty(this.0.first_of_year()))
        });
        method($methods, "last_of_year", |_, this: &$ty, ()| {
            Ok($ty(this.0.last_of_year()))
        });
        date_moves!($methods, $ty);
    };
    ($methods:ident, $ty:ident, fallible) => {
        method($methods, "first_of_month", |_, this: &$ty, ()| {
            this.0.first_of_month().or_raise().map($ty)
        });
        method($methods, "last_of_month", |_, this: &$ty, ()| {
            this.0.last_of_month().or_raise().map($ty)
        });
        method($methods, "first_of_year", |_, this: &$ty, ()| {
            this.0.first_of_year().or_raise().map($ty)
        });
        method($methods, "last_of_year", |_, this: &$ty, ()| {
            this.0.last_of_year().or_raise().map($ty)
        });
        date_moves!($methods, $ty);
    };
    ($methods:ident, $ty:ident) => {
        method($methods, "tomorrow", |_, this: &$ty, ()| {
            this.0.tomorrow().or_raise().map($ty)
        });
        method($methods, "yesterday", |_, this: &$ty, ()| {
            this.0.yesterday().or_raise().map($ty)
        });
        method(
            $methods,
            "nth_weekday",
            |what, this: &$ty, (nth, day): (Value, Value)| {
                let nth = narrow(&nth, what, "nth")?;
                let day = weekday(&day, what)?;
                this.0.nth_weekday(nth, day).or_raise().map($ty)
            },
        );
        method(
            $methods,
            "nth_weekday_of_month",
            |what, this: &$ty, (nth, day): (Value, Value)| {
                let nth = narrow(&nth, what, "nth")?;
                let day = weekday(&day, what)?;
                this.0.nth_weekday_of_month(nth, day).or_raise().map($ty)
            },
        );
    };
}

/// The getters jiff gives a time of day.
macro_rules! time_getters {
    ($methods:ident, $ty:ident) => {
        method($methods, "hour", |_, this: &$ty, ()| Ok(this.0.hour()));
        method($methods, "minute", |_, this: &$ty, ()| Ok(this.0.minute()));
        method($methods, "second", |_, this: &$ty, ()| Ok(this.0.second()));
        method($methods, "millisecond", |_, this: &$ty, ()| {
            Ok(this.0.millisecond())
        });
        method($methods, "microsecond", |_, this: &$ty, ()| {
            Ok(this.0.microsecond())
        });
        method($methods, "nanosecond", |_, this: &$ty, ()| {
            Ok(this.0.nanosecond())
        });
        method($methods, "subsec_nanosecond", |_, this: &$ty, ()| {
            Ok(this.0.subsec_nanosecond())
        });
    };
}

// -- `with` ------------------------------------------------------------------------------------

/// What a `with` table's field does to one of jiff's `*With` builders: `Ok` with the builder
/// the field was applied to, or `Err` handing the builder back when the field is not this one's.
type Field<W> = Result<std::result::Result<W, W>>;

/// Applies a `with` table to `builder`, one field at a time, raising on a key `apply` hands back.
fn build_with<W>(
    builder: W,
    fields: &Value,
    keys: &[&str],
    what: &str,
    mut apply: impl FnMut(W, &str, &Value) -> Field<W>,
) -> Result<W> {
    let fields = table_arg(fields, what)?;
    let mut builder = Some(builder);
    each_field(&fields, what, keys, |key, v| {
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

const DATE_FIELDS: &[&str] = &["year", "month", "day", "day_of_year", "day_of_year_no_leap"];
const TIME_FIELDS: &[&str] = &[
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
    "subsec_nanosecond",
];

/// A `with` table's date fields, applied to one of jiff's `*With` builders.
fn date_field<W: DateFields>(w: W, key: &str, v: &Value, what: &str) -> Field<W> {
    Ok(Ok(match key {
        "year" => w.year(narrow(v, what, key)?),
        "month" => w.month(narrow(v, what, key)?),
        "day" => w.day(narrow(v, what, key)?),
        "day_of_year" => w.day_of_year(narrow(v, what, key)?),
        "day_of_year_no_leap" => w.day_of_year_no_leap(narrow(v, what, key)?),
        _ => return Ok(Err(w)),
    }))
}

/// A `with` table's time fields, applied to one of jiff's `*With` builders.
fn time_field<W: TimeFields>(w: W, key: &str, v: &Value, what: &str) -> Field<W> {
    Ok(Ok(match key {
        "hour" => w.hour(narrow(v, what, key)?),
        "minute" => w.minute(narrow(v, what, key)?),
        "second" => w.second(narrow(v, what, key)?),
        "millisecond" => w.millisecond(narrow(v, what, key)?),
        "microsecond" => w.microsecond(narrow(v, what, key)?),
        "nanosecond" => w.nanosecond(narrow(v, what, key)?),
        "subsec_nanosecond" => w.subsec_nanosecond(narrow(v, what, key)?),
        _ => return Ok(Err(w)),
    }))
}

/// A `with` table's date and time fields, and `date` and `time` whole, for the builders that
/// have both.
fn datetime_field<W: DateFields + TimeFields + WholeFields>(
    w: W,
    key: &str,
    v: &Value,
    what: &str,
) -> Field<W> {
    Ok(Ok(match key {
        "date" => w.date(userdata::<Date>(v, what)?.0),
        "time" => w.time(userdata::<Time>(v, what)?.0),
        _ => match date_field(w, key, v, what)? {
            Ok(w) => w,
            Err(w) => return time_field(w, key, v, what),
        },
    }))
}

trait WholeFields: Sized {
    fn date(self, v: civil::Date) -> Self;
    fn time(self, v: civil::Time) -> Self;
}

impl WholeFields for civil::DateTimeWith {
    fn date(self, v: civil::Date) -> Self {
        civil::DateTimeWith::date(self, v)
    }
    fn time(self, v: civil::Time) -> Self {
        civil::DateTimeWith::time(self, v)
    }
}

impl WholeFields for jiff::ZonedWith {
    fn date(self, v: civil::Date) -> Self {
        jiff::ZonedWith::date(self, v)
    }
    fn time(self, v: civil::Time) -> Self {
        jiff::ZonedWith::time(self, v)
    }
}

// -- Timestamp ---------------------------------------------------------------------------------

wrapper!(
    /// An instant, with no time zone: jiff's `Timestamp`.
    Timestamp(jiff::Timestamp), copy
);

impl UserData for Timestamp {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, Timestamp, ordered);
        arithmetic!(methods, Timestamp, jiff::TimestampDifference);
        rounding!(methods, Timestamp, jiff::TimestampRound);
        method(methods, "as_second", |_, this: &Self, ()| {
            Ok(this.0.as_second())
        });
        method(methods, "as_millisecond", |_, this: &Self, ()| {
            Ok(this.0.as_millisecond())
        });
        method(methods, "as_microsecond", |_, this: &Self, ()| {
            Ok(this.0.as_microsecond())
        });
        method(methods, "as_nanosecond", |what, this: &Self, ()| {
            i128_to_i64(this.0.as_nanosecond(), what)
        });
        method(methods, "subsec_millisecond", |_, this: &Self, ()| {
            Ok(this.0.subsec_millisecond())
        });
        method(methods, "subsec_microsecond", |_, this: &Self, ()| {
            Ok(this.0.subsec_microsecond())
        });
        method(methods, "subsec_nanosecond", |_, this: &Self, ()| {
            Ok(this.0.subsec_nanosecond())
        });
        method(methods, "signum", |_, this: &Self, ()| Ok(this.0.signum()));
        method(methods, "is_zero", |_, this: &Self, ()| {
            Ok(this.0.is_zero())
        });
        method(methods, "as_duration", |_, this: &Self, ()| {
            Ok(SignedDuration(this.0.as_duration()))
        });
        method(methods, "to_zoned", |what, this: &Self, zone: Value| {
            let zone = userdata::<TimeZone>(&zone, what)?;
            Ok(Zoned(this.0.to_zoned(zone.0)))
        });
        method(methods, "in_tz", |what, this: &Self, name: Value| {
            this.0.in_tz(&string(&name, what)?).or_raise().map(Zoned)
        });
    }
}

fn timestamp_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "Timestamp")?;
    t.add("now", |_, ()| Ok(Timestamp(jiff::Timestamp::now())))?;
    t.add("new", |what, (second, nanosecond): (Value, Value)| {
        let second = integer(&second, what)?;
        let nanosecond = narrow_or(&nanosecond, 0, what, "nanosecond")?;
        jiff::Timestamp::new(second, nanosecond)
            .or_raise()
            .map(Timestamp)
    })?;
    t.add("from_second", |what, n: Value| {
        jiff::Timestamp::from_second(integer(&n, what)?)
            .or_raise()
            .map(Timestamp)
    })?;
    t.add("from_millisecond", |what, n: Value| {
        jiff::Timestamp::from_millisecond(integer(&n, what)?)
            .or_raise()
            .map(Timestamp)
    })?;
    t.add("from_microsecond", |what, n: Value| {
        jiff::Timestamp::from_microsecond(integer(&n, what)?)
            .or_raise()
            .map(Timestamp)
    })?;
    t.add("from_nanosecond", |what, n: Value| {
        jiff::Timestamp::from_nanosecond(integer(&n, what)?.into())
            .or_raise()
            .map(Timestamp)
    })?;
    t.add("from_duration", |what, d: Value| {
        let d = userdata::<SignedDuration>(&d, what)?;
        jiff::Timestamp::from_duration(d.0)
            .or_raise()
            .map(Timestamp)
    })?;
    t.parsers(Timestamp, |f, s| jiff::Timestamp::strptime(f, s))?;
    t.done()
}

// -- Zoned -------------------------------------------------------------------------------------

wrapper!(
    /// An instant in a time zone: jiff's `Zoned`.
    Zoned(jiff::Zoned), ref
);

impl UserData for Zoned {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, Zoned, ordered);
        arithmetic!(methods, Zoned, jiff::ZonedDifference);
        rounding!(methods, Zoned, jiff::ZonedRound);
        date_getters!(methods, Zoned);
        time_getters!(methods, Zoned);
        date_moves!(methods, Zoned, fallible);
        method(methods, "start_of_day", |_, this: &Self, ()| {
            this.0.start_of_day().or_raise().map(Zoned)
        });
        method(methods, "end_of_day", |_, this: &Self, ()| {
            this.0.end_of_day().or_raise().map(Zoned)
        });
        method(methods, "timestamp", |_, this: &Self, ()| {
            Ok(Timestamp(this.0.timestamp()))
        });
        method(methods, "datetime", |_, this: &Self, ()| {
            Ok(DateTime(this.0.datetime()))
        });
        method(methods, "date", |_, this: &Self, ()| {
            Ok(Date(this.0.date()))
        });
        method(methods, "time", |_, this: &Self, ()| {
            Ok(Time(this.0.time()))
        });
        method(methods, "time_zone", |_, this: &Self, ()| {
            Ok(TimeZone(this.0.time_zone().clone()))
        });
        method(
            methods,
            "with_time_zone",
            |what, this: &Self, zone: Value| {
                let zone = userdata::<TimeZone>(&zone, what)?;
                Ok(Zoned(this.0.with_time_zone(zone.0)))
            },
        );
        method(methods, "in_tz", |what, this: &Self, name: Value| {
            this.0.in_tz(&string(&name, what)?).or_raise().map(Zoned)
        });
        method(methods, "with", |what, this: &Self, fields: Value| {
            let keys = known(&[
                DATE_FIELDS,
                TIME_FIELDS,
                &["date", "time", "disambiguation"],
            ]);
            build_with(this.0.with(), &fields, &keys, what, |w, key, v| {
                if key == "disambiguation" {
                    return Ok(Ok(w.disambiguation(disambiguation(v, what)?)));
                }
                datetime_field(w, key, v, what)
            })?
            .build()
            .or_raise()
            .map(Zoned)
        });
    }
}

fn zoned_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "Zoned")?;
    t.add("now", |_, ()| Ok(Zoned(jiff::Zoned::now())))?;
    t.add("new", |what, (ts, zone): (Value, Value)| {
        let ts = userdata::<Timestamp>(&ts, what)?;
        let zone = userdata::<TimeZone>(&zone, what)?;
        Ok(Zoned(jiff::Zoned::new(ts.0, zone.0)))
    })?;
    t.parsers(Zoned, |f, s| jiff::Zoned::strptime(f, s))?;
    t.done()
}

// -- Date --------------------------------------------------------------------------------------

wrapper!(
    /// A calendar date, with no time or time zone: jiff's `civil::Date`.
    Date(civil::Date), copy
);

fn new_date((year, month, day): (Value, Value, Value), what: &str) -> Result<Date> {
    civil::Date::new(
        narrow(&year, what, "year")?,
        narrow(&month, what, "month")?,
        narrow(&day, what, "day")?,
    )
    .or_raise()
    .map(Date)
}

/// `(hour, minute, second, nanosecond)`, the last two optional, as a `civil::Time`.
fn time_of(
    (hour, minute, second, nanosecond): (Value, Value, Value, Value),
    what: &str,
) -> Result<civil::Time> {
    civil::Time::new(
        narrow(&hour, what, "hour")?,
        narrow(&minute, what, "minute")?,
        narrow_or(&second, 0, what, "second")?,
        narrow_or(&nanosecond, 0, what, "nanosecond")?,
    )
    .or_raise()
}

impl UserData for Date {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, Date, ordered);
        arithmetic!(methods, Date, civil::DateDifference);
        date_getters!(methods, Date);
        date_moves!(methods, Date, infallible);
        method(methods, "at", |what, this: &Self, args| {
            Ok(DateTime(this.0.to_datetime(time_of(args, what)?)))
        });
        method(methods, "to_datetime", |what, this: &Self, time: Value| {
            let time = userdata::<Time>(&time, what)?;
            Ok(DateTime(this.0.to_datetime(time.0)))
        });
        method(
            methods,
            "to_zoned",
            |what, this: &Self, (zone, options): (Value, Value)| {
                let zone = userdata::<TimeZone>(&zone, what)?;
                let midnight = this.0.to_datetime(civil::Time::midnight());
                to_zoned(&zone.0, midnight, &options, what).map(Zoned)
            },
        );
        method(
            methods,
            "in_tz",
            |what, this: &Self, (name, options): (Value, Value)| {
                let midnight = this.0.to_datetime(civil::Time::midnight());
                in_tz(&name, midnight, &options, what).map(Zoned)
            },
        );
        method(methods, "with", |what, this: &Self, fields: Value| {
            build_with(this.0.with(), &fields, DATE_FIELDS, what, |w, k, v| {
                date_field(w, k, v, what)
            })?
            .build()
            .or_raise()
            .map(Date)
        });
    }
}

fn date_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "Date")?;
    t.add("new", |what, args| new_date(args, what))?;
    t.parsers(Date, |f, s| civil::Date::strptime(f, s))?;
    t.done()
}

// -- Time --------------------------------------------------------------------------------------

wrapper!(
    /// A time of day, with no date or time zone: jiff's `civil::Time`.
    Time(civil::Time), copy
);

fn new_time(args: (Value, Value, Value, Value), what: &str) -> Result<Time> {
    time_of(args, what).map(Time)
}

impl UserData for Time {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, Time, ordered);
        arithmetic!(methods, Time, civil::TimeDifference);
        rounding!(methods, Time, civil::TimeRound);
        time_getters!(methods, Time);
        method(methods, "on", |what, this: &Self, args| {
            Ok(DateTime(new_date(args, what)?.0.to_datetime(this.0)))
        });
        method(methods, "to_datetime", |what, this: &Self, date: Value| {
            let date = userdata::<Date>(&date, what)?;
            Ok(DateTime(this.0.to_datetime(date.0)))
        });
        method(methods, "with", |what, this: &Self, fields: Value| {
            build_with(this.0.with(), &fields, TIME_FIELDS, what, |w, k, v| {
                time_field(w, k, v, what)
            })?
            .build()
            .or_raise()
            .map(Time)
        });
    }
}

fn time_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "Time")?;
    t.add("new", |what, args| new_time(args, what))?;
    t.add("midnight", |_, ()| Ok(Time(civil::Time::midnight())))?;
    t.parsers(Time, |f, s| civil::Time::strptime(f, s))?;
    t.done()
}

// -- DateTime ----------------------------------------------------------------------------------

wrapper!(
    /// A date and a time of day, with no time zone: jiff's `civil::DateTime`.
    DateTime(civil::DateTime), copy
);

/// `(year, month, day, hour, minute, second, nanosecond)`, everything after the day optional.
fn new_datetime(
    (year, month, day, hour, minute, second, nanosecond): (
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
    ),
    what: &str,
) -> Result<DateTime> {
    let date = new_date((year, month, day), what)?;
    let or_zero = |v: Value| if v.is_nil() { Value::Integer(0) } else { v };
    let time = time_of((or_zero(hour), or_zero(minute), second, nanosecond), what)?;
    Ok(DateTime(date.0.to_datetime(time)))
}

impl UserData for DateTime {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, DateTime, ordered);
        arithmetic!(methods, DateTime, civil::DateTimeDifference);
        rounding!(methods, DateTime, civil::DateTimeRound);
        date_getters!(methods, DateTime);
        time_getters!(methods, DateTime);
        date_moves!(methods, DateTime, infallible);
        method(methods, "start_of_day", |_, this: &Self, ()| {
            Ok(DateTime(this.0.start_of_day()))
        });
        method(methods, "end_of_day", |_, this: &Self, ()| {
            Ok(DateTime(this.0.end_of_day()))
        });
        method(methods, "date", |_, this: &Self, ()| {
            Ok(Date(this.0.date()))
        });
        method(methods, "time", |_, this: &Self, ()| {
            Ok(Time(this.0.time()))
        });
        method(
            methods,
            "to_zoned",
            |what, this: &Self, (zone, options): (Value, Value)| {
                let zone = userdata::<TimeZone>(&zone, what)?;
                to_zoned(&zone.0, this.0, &options, what).map(Zoned)
            },
        );
        method(
            methods,
            "in_tz",
            |what, this: &Self, (name, options): (Value, Value)| {
                in_tz(&name, this.0, &options, what).map(Zoned)
            },
        );
        method(methods, "with", |what, this: &Self, fields: Value| {
            let keys = known(&[DATE_FIELDS, TIME_FIELDS, &["date", "time"]]);
            build_with(this.0.with(), &fields, &keys, what, |w, k, v| {
                datetime_field(w, k, v, what)
            })?
            .build()
            .or_raise()
            .map(DateTime)
        });
    }
}

fn datetime_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "DateTime")?;
    t.add("new", |what, args| new_datetime(args, what))?;
    t.parsers(DateTime, |f, s| civil::DateTime::strptime(f, s))?;
    t.done()
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

/// Sets one of a span's units, by its plural name, as jiff's `try_*` setters do. `None` for a
/// name that is not a unit.
fn set_unit(span: jiff::Span, unit: &str, v: &Value, what: &str) -> Result<Option<jiff::Span>> {
    let n = || integer(v, what);
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

fn span_with(span: jiff::Span, fields: &Table, what: &str) -> Result<jiff::Span> {
    let mut span = span;
    each_field(fields, what, SPAN_UNITS, |key, v| {
        match set_unit(span, key, &v, what)? {
            Some(s) => span = s,
            None => return Ok(false),
        }
        Ok(true)
    })?;
    Ok(span)
}

/// What a span's calendar units are measured from, for the methods that need one: `relative`, a
/// `Date`, `DateTime` or `Zoned`, or `days_are_24_hours`, which gives days a fixed length and
/// leaves weeks, months and years with none.
enum Relative {
    Date(civil::Date),
    DateTime(civil::DateTime),
    Zoned(jiff::Zoned),
    DaysAre24Hours,
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

    fn span_relative_to(&self) -> jiff::SpanRelativeTo<'_> {
        match self {
            Relative::Date(d) => (*d).into(),
            Relative::DateTime(d) => (*d).into(),
            Relative::Zoned(z) => z.into(),
            Relative::DaysAre24Hours => jiff::SpanRelativeTo::days_are_24_hours(),
        }
    }
}

/// A span method's options: `relative` or `days_are_24_hours`, and `rounding_keys` besides.
fn span_options(
    options: &Value,
    rounding_keys: &[&str],
    what: &str,
) -> Result<(Option<Relative>, Rounding)> {
    let mut relative = None;
    let mut days24 = false;
    let mut r = Rounding::default();
    if options.is_nil() {
        return Ok((relative, r));
    }
    let keys = known(&[&["relative", "days_are_24_hours"], rounding_keys]);
    each_field(&table_arg(options, what)?, what, &keys, |key, v| {
        match key {
            "relative" => relative = Some(Relative::from(&v, what)?),
            "days_are_24_hours" => match v {
                Value::Boolean(b) => days24 = b,
                other => {
                    return Err(raise(format!(
                        "{what}: days_are_24_hours must be a boolean, got {}",
                        describe(&other)
                    )));
                }
            },
            _ => return r.field(rounding_keys, key, &v, what),
        }
        Ok(true)
    })?;
    if days24 {
        if relative.is_some() {
            return Err(raise(format!(
                "{what}: give relative or days_are_24_hours, not both"
            )));
        }
        relative = Some(Relative::DaysAre24Hours);
    }
    Ok((relative, r))
}

/// `other`, a span or a signed duration, as a span.
fn span_operand(other: &Value, what: &str) -> Result<jiff::Span> {
    match span_or_duration(other, what)? {
        SpanOrDuration::Span(s) => Ok(s),
        SpanOrDuration::Duration(d) => jiff::Span::try_from(d).or_raise(),
    }
}

fn span_add(this: jiff::Span, other: jiff::Span, options: &Value, what: &str) -> Result<Span> {
    let (relative, _) = span_options(options, &[], what)?;
    match &relative {
        Some(r) => this.checked_add((other, r.span_relative_to())),
        None => this.checked_add(other),
    }
    .or_raise()
    .map(Span)
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
            let what = "Span *";
            let (span, n) = if is::<Span>(&a) { (a, b) } else { (b, a) };
            let span = userdata::<Span>(&span, what)?;
            span.0.checked_mul(integer(&n, what)?).or_raise().map(Span)
        });
        macro_rules! units {
            ($($get:ident $set:ident),*) => {$(
                method(methods, stringify!($get), |_, this: &Self, ()| Ok(this.0.$get()));
                method(methods, stringify!($set), |what, this: &Self, n: Value| {
                    let span = set_unit(this.0, stringify!($set), &n, what)?;
                    Ok(Span(span.expect("a unit")))
                });
            )*};
        }
        units!(
            get_years years, get_months months, get_weeks weeks, get_days days,
            get_hours hours, get_minutes minutes, get_seconds seconds,
            get_milliseconds milliseconds, get_microseconds microseconds,
            get_nanoseconds nanoseconds
        );
        method(methods, "abs", |_, this: &Self, ()| Ok(Span(this.0.abs())));
        method(methods, "negate", |_, this: &Self, ()| {
            Ok(Span(this.0.negate()))
        });
        method(methods, "signum", |_, this: &Self, ()| Ok(this.0.signum()));
        method(methods, "is_zero", |_, this: &Self, ()| {
            Ok(this.0.is_zero())
        });
        method(methods, "is_negative", |_, this: &Self, ()| {
            Ok(this.0.is_negative())
        });
        method(methods, "is_positive", |_, this: &Self, ()| {
            Ok(this.0.is_positive())
        });
        method(methods, "mul", |what, this: &Self, n: Value| {
            this.0.checked_mul(integer(&n, what)?).or_raise().map(Span)
        });
        method(
            methods,
            "add",
            |what, this: &Self, (other, options): (Value, Value)| {
                let other = span_operand(&other, what)?;
                span_add(this.0, other, &options, what)
            },
        );
        method(
            methods,
            "sub",
            |what, this: &Self, (other, options): (Value, Value)| {
                let other = span_operand(&other, what)?;
                span_add(this.0, other.negate(), &options, what)
            },
        );
        method(
            methods,
            "compare",
            |what, this: &Self, (other, options): (Value, Value)| {
                let other = span_operand(&other, what)?;
                let (relative, _) = span_options(&options, &[], what)?;
                let ordering = match &relative {
                    Some(r) => this.0.compare((other, r.span_relative_to())),
                    None => this.0.compare(other),
                };
                Ok(ordering.or_raise()? as i8)
            },
        );
        method(
            methods,
            "total",
            |what, this: &Self, (u, options): (Value, Value)| {
                let u = unit(&u, what)?;
                let (relative, _) = span_options(&options, &[], what)?;
                match &relative {
                    Some(r) => this.0.total((u, r.span_relative_to())),
                    None => this.0.total(u),
                }
                .or_raise()
            },
        );
        method(methods, "round", |what, this: &Self, options: Value| {
            let (relative, r) = match &options {
                Value::String(_) => (None, Rounding::from(&options, DIFFERENCE_KEYS, true, what)?),
                other => span_options(other, DIFFERENCE_KEYS, what)?,
            };
            let round = configure!(jiff::SpanRound::new(), r, largest);
            match &relative {
                Some(rel) => this.0.round(round.relative(rel.span_relative_to())),
                None => this.0.round(round),
            }
            .or_raise()
            .map(Span)
        });
        method(
            methods,
            "to_duration",
            |what, this: &Self, options: Value| {
                let (relative, _) = span_options(&options, &[], what)?;
                match &relative {
                    Some(r) => this.0.to_duration(r.span_relative_to()),
                    None => jiff::SignedDuration::try_from(this.0),
                }
                .or_raise()
                .map(SignedDuration)
            },
        );
    }
}

fn span_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "Span")?;
    t.add("new", |_, ()| Ok(Span(jiff::Span::new())))?;
    t.add("parse", |what, s: Value| {
        string(&s, what)?.parse().or_raise().map(Span)
    })?;
    t.done()
}

// -- SignedDuration ----------------------------------------------------------------------------

/// An exact length of time, in seconds and nanoseconds: jiff's `SignedDuration`.
#[derive(Clone, Copy)]
pub struct SignedDuration(jiff::SignedDuration);

fn overflow(what: &str) -> mlua::Error {
    raise(format!("{what}: the result overflows a SignedDuration"))
}

/// The arithmetic a SignedDuration's methods and operators share, `what` naming whichever it is.
fn duration_add(a: jiff::SignedDuration, b: &Value, what: &str) -> Result<SignedDuration> {
    a.checked_add(userdata::<SignedDuration>(b, what)?.0)
        .map(SignedDuration)
        .ok_or_else(|| overflow(what))
}

fn duration_sub(a: jiff::SignedDuration, b: &Value, what: &str) -> Result<SignedDuration> {
    a.checked_sub(userdata::<SignedDuration>(b, what)?.0)
        .map(SignedDuration)
        .ok_or_else(|| overflow(what))
}

fn duration_mul(d: jiff::SignedDuration, n: &Value, what: &str) -> Result<SignedDuration> {
    d.checked_mul(narrow(n, what, "")?)
        .map(SignedDuration)
        .ok_or_else(|| overflow(what))
}

fn duration_div(d: jiff::SignedDuration, n: &Value, what: &str) -> Result<SignedDuration> {
    let n: i32 = narrow(n, what, "")?;
    if n == 0 {
        return Err(raise(format!("{what}: division by zero")));
    }
    d.checked_div(n)
        .map(SignedDuration)
        .ok_or_else(|| overflow(what))
}

fn duration_neg(d: jiff::SignedDuration, what: &str) -> Result<SignedDuration> {
    d.checked_neg()
        .map(SignedDuration)
        .ok_or_else(|| overflow(what))
}

impl UserData for SignedDuration {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        display_and_compare!(methods, SignedDuration, ordered);
        methods.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            let what = "SignedDuration +";
            duration_add(userdata::<SignedDuration>(&a, what)?.0, &b, what)
        });
        methods.add_meta_function(MetaMethod::Sub, |_, (a, b): (Value, Value)| {
            let what = "SignedDuration -";
            duration_sub(userdata::<SignedDuration>(&a, what)?.0, &b, what)
        });
        methods.add_meta_method(MetaMethod::Unm, |_, this, ()| {
            duration_neg(this.0, "SignedDuration -")
        });
        // Either way round: `d * 2` and `2 * d`.
        methods.add_meta_function(MetaMethod::Mul, |_, (a, b): (Value, Value)| {
            let what = "SignedDuration *";
            let (d, n) = if is::<SignedDuration>(&a) {
                (a, b)
            } else {
                (b, a)
            };
            duration_mul(userdata::<SignedDuration>(&d, what)?.0, &n, what)
        });
        methods.add_meta_function(MetaMethod::Div, |_, (a, b): (Value, Value)| {
            let what = "SignedDuration /";
            duration_div(userdata::<SignedDuration>(&a, what)?.0, &b, what)
        });
        method(methods, "add", |what, this: &Self, other: Value| {
            duration_add(this.0, &other, what)
        });
        method(methods, "sub", |what, this: &Self, other: Value| {
            duration_sub(this.0, &other, what)
        });
        method(methods, "mul", |what, this: &Self, n: Value| {
            duration_mul(this.0, &n, what)
        });
        method(methods, "div", |what, this: &Self, n: Value| {
            duration_div(this.0, &n, what)
        });
        method(methods, "neg", |what, this: &Self, ()| {
            duration_neg(this.0, what)
        });
        method(methods, "abs", |what, this: &Self, ()| {
            if this.0 == jiff::SignedDuration::MIN {
                return Err(overflow(what));
            }
            Ok(SignedDuration(this.0.abs()))
        });
        method(methods, "as_secs", |_, this: &Self, ()| {
            Ok(this.0.as_secs())
        });
        method(methods, "as_mins", |_, this: &Self, ()| {
            Ok(this.0.as_mins())
        });
        method(methods, "as_hours", |_, this: &Self, ()| {
            Ok(this.0.as_hours())
        });
        method(methods, "as_millis", |what, this: &Self, ()| {
            i128_to_i64(this.0.as_millis(), what)
        });
        method(methods, "as_micros", |what, this: &Self, ()| {
            i128_to_i64(this.0.as_micros(), what)
        });
        method(methods, "as_nanos", |what, this: &Self, ()| {
            i128_to_i64(this.0.as_nanos(), what)
        });
        method(methods, "as_secs_f64", |_, this: &Self, ()| {
            Ok(this.0.as_secs_f64())
        });
        method(methods, "as_millis_f64", |_, this: &Self, ()| {
            Ok(this.0.as_millis_f64())
        });
        method(methods, "subsec_millis", |_, this: &Self, ()| {
            Ok(this.0.subsec_millis())
        });
        method(methods, "subsec_micros", |_, this: &Self, ()| {
            Ok(this.0.subsec_micros())
        });
        method(methods, "subsec_nanos", |_, this: &Self, ()| {
            Ok(this.0.subsec_nanos())
        });
        method(methods, "is_zero", |_, this: &Self, ()| {
            Ok(this.0.is_zero())
        });
        method(methods, "is_negative", |_, this: &Self, ()| {
            Ok(this.0.is_negative())
        });
        method(methods, "is_positive", |_, this: &Self, ()| {
            Ok(this.0.is_positive())
        });
        method(methods, "signum", |_, this: &Self, ()| Ok(this.0.signum()));
        method(methods, "round", |what, this: &Self, options: Value| {
            let r = Rounding::from(&options, ROUND_KEYS, true, what)?;
            this.0
                .round(configure!(jiff::SignedDurationRound::new(), r))
                .or_raise()
                .map(SignedDuration)
        });
    }
}

fn signed_duration_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "SignedDuration")?;
    t.add("new", |what, (secs, nanos): (Value, Value)| {
        let secs = integer(&secs, what)?;
        let nanos: i32 = narrow_or(&nanos, 0, what, "nanos")?;
        // jiff's `new` panics where the nanoseconds carry the seconds past `i64`.
        let carry = i64::from(nanos / 1_000_000_000);
        secs.checked_add(carry)
            .map(|_| SignedDuration(jiff::SignedDuration::new(secs, nanos)))
            .ok_or_else(|| overflow(what))
    })?;
    macro_rules! from {
        ($($name:ident),*) => {$(
            t.add(stringify!($name), |what, n: Value| {
                Ok(SignedDuration(jiff::SignedDuration::$name(integer(&n, what)?)))
            })?;
        )*};
    }
    from!(from_secs, from_millis, from_micros, from_nanos);
    t.add("from_mins", |what, n: Value| {
        jiff::SignedDuration::try_from_mins(integer(&n, what)?)
            .ok_or_else(|| overflow(what))
            .map(SignedDuration)
    })?;
    t.add("from_hours", |what, n: Value| {
        jiff::SignedDuration::try_from_hours(integer(&n, what)?)
            .ok_or_else(|| overflow(what))
            .map(SignedDuration)
    })?;
    t.add("from_secs_f64", |what, n: Value| {
        jiff::SignedDuration::try_from_secs_f64(float(&n, what)?)
            .or_raise()
            .map(SignedDuration)
    })?;
    t.add("parse", |what, s: Value| {
        string(&s, what)?.parse().or_raise().map(SignedDuration)
    })?;
    t.set("ZERO", SignedDuration(jiff::SignedDuration::ZERO))?;
    t.done()
}

// -- TimeZone ----------------------------------------------------------------------------------

/// A set of rules for turning an instant into a civil time: jiff's `TimeZone`.
#[derive(Clone)]
pub struct TimeZone(tz::TimeZone);

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
        method(methods, "iana_name", |_, this: &Self, ()| {
            Ok(this.0.iana_name().map(str::to_owned))
        });
        method(
            methods,
            "to_zoned",
            |what, this: &Self, (dt, options): (Value, Value)| {
                let dt = userdata::<DateTime>(&dt, what)?;
                to_zoned(&this.0, dt.0, &options, what).map(Zoned)
            },
        );
        method(methods, "to_datetime", |what, this: &Self, ts: Value| {
            let ts = userdata::<Timestamp>(&ts, what)?;
            Ok(DateTime(this.0.to_datetime(ts.0)))
        });
    }
}

fn time_zone_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "TimeZone")?;
    t.add("get", |what, name: Value| {
        tz::TimeZone::get(&string(&name, what)?)
            .or_raise()
            .map(TimeZone)
    })?;
    t.add("system", |_, ()| Ok(TimeZone(tz::TimeZone::system())))?;
    t.add("try_system", |_, ()| {
        tz::TimeZone::try_system().or_raise().map(TimeZone)
    })?;
    t.set("UTC", TimeZone(tz::TimeZone::UTC))?;
    t.done()
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
        method(methods, "to_monday_zero_offset", |_, this: &Self, ()| {
            Ok(this.0.to_monday_zero_offset())
        });
        method(methods, "to_monday_one_offset", |_, this: &Self, ()| {
            Ok(this.0.to_monday_one_offset())
        });
        method(methods, "to_sunday_zero_offset", |_, this: &Self, ()| {
            Ok(this.0.to_sunday_zero_offset())
        });
        method(methods, "to_sunday_one_offset", |_, this: &Self, ()| {
            Ok(this.0.to_sunday_one_offset())
        });
        method(methods, "next", |_, this: &Self, ()| {
            Ok(Weekday(this.0.next()))
        });
        method(methods, "previous", |_, this: &Self, ()| {
            Ok(Weekday(this.0.previous()))
        });
        // jiff's `until` and `since`: `until` is a Lua keyword, so `w:until(x)` would not parse.
        method(methods, "days_until", |what, this: &Self, other: Value| {
            Ok(this.0.until(weekday(&other, what)?))
        });
        method(methods, "days_since", |what, this: &Self, other: Value| {
            Ok(this.0.since(weekday(&other, what)?))
        });
        method(methods, "wrapping_add", |what, this: &Self, n: Value| {
            Ok(Weekday(this.0.wrapping_add(integer(&n, what)?)))
        });
        method(methods, "wrapping_sub", |what, this: &Self, n: Value| {
            Ok(Weekday(this.0.wrapping_sub(integer(&n, what)?)))
        });
    }
}

fn weekday_table(lua: &Lua) -> Result<Table> {
    let t = Constructors::new(lua, "Weekday")?;
    for (day, name) in WEEKDAYS {
        t.set(name.to_ascii_lowercase(), Weekday(day))?;
    }
    t.add("from_name", |what, name: Value| {
        weekday_from_name(&string(&name, what)?, what).map(Weekday)
    })?;
    macro_rules! offsets {
        ($($from:ident),*) => {$(
            t.add(stringify!($from), |what, n: Value| {
                civil::Weekday::$from(narrow(&n, what, "")?)
                    .or_raise()
                    .map(Weekday)
            })?;
        )*};
    }
    offsets!(
        from_monday_zero_offset,
        from_monday_one_offset,
        from_sunday_zero_offset,
        from_sunday_one_offset
    );
    t.done()
}

// -- sleep -------------------------------------------------------------------------------------

/// What `sleep` waits for: a SignedDuration, a Span with no calendar units, or a whole number of
/// milliseconds. A negative length raises rather than returning at once, since it is a mistake.
fn sleep_duration(amount: &Value, what: &str) -> Result<std::time::Duration> {
    let signed = match amount {
        Value::Integer(_) | Value::Number(_) => {
            jiff::SignedDuration::from_millis(integer(amount, what)?)
        }
        _ => match span_or_duration(amount, what).map_err(|_| {
            raise(format!(
                "{what}: expected a SignedDuration, a Span or a number of milliseconds, got {}",
                describe(amount)
            ))
        })? {
            SpanOrDuration::Duration(d) => d,
            SpanOrDuration::Span(s) => jiff::SignedDuration::try_from(s).map_err(|_| {
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
