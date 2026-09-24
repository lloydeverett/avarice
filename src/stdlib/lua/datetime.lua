-- Original to avarice-rt: not derived from Astra. See ADR 0019.
--
-- Dates and times, built on jiff <https://docs.rs/jiff>. Every type here is one of jiff's, with
-- jiff's method names, and jiff's documentation is the reference for what each does. The
-- differences are Lua's:
--   - Nothing is spelt `checked_`: every method raises on failure.
--   - jiff's builders are an optional options table, e.g. `a:span_until(b, { largest = "year" })`.
--   - Values are immutable. `v:with { day = 1 }` returns a changed copy.
--   - `until` is a Lua keyword, so jiff's `until` and `since` are `span_until` and `span_since`,
--     beside `duration_until` and `duration_since`, and a Weekday's are `days_until` and
--     `days_since`.
--   - Numbers are integers. A float with a fractional part raises, except where jiff takes or gives
--     an `f64`.
--   - An argument past the last one a function takes raises, where Lua would drop it.
-- Units are jiff's in lower case: "year", "month", "week", "day", "hour", "minute", "second",
-- "millisecond", "microsecond", "nanosecond". Rounding modes are too, in snake case: "ceil",
-- "floor", "expand", "trunc", "half_ceil", "half_floor", "half_expand", "half_trunc", "half_even".

---@meta

---@alias datetime.Unit "year"|"month"|"week"|"day"|"hour"|"minute"|"second"|"millisecond"|"microsecond"|"nanosecond"
---@alias datetime.RoundMode "ceil"|"floor"|"expand"|"trunc"|"half_ceil"|"half_floor"|"half_expand"|"half_trunc"|"half_even"
---@alias datetime.Duration datetime.Span|datetime.SignedDuration
---A Weekday, or its English name, full or three letters, in any case: "Monday", "mon".
---@alias datetime.WeekdayLike datetime.Weekday|string

---What `span_until` and `span_since` take.
---@class datetime.DifferenceOptions
---@field largest? datetime.Unit
---@field smallest? datetime.Unit
---@field mode? datetime.RoundMode
---@field increment? integer

---What `round` takes: this, or a unit on its own.
---@class datetime.RoundOptions
---@field smallest? datetime.Unit
---@field mode? datetime.RoundMode
---@field increment? integer

---How a civil time that a DST change skips or repeats becomes a zoned one. The default is
---"compatible": a skipped time moves forward, and a repeated one takes the earlier offset.
---@class datetime.ZonedOptions
---@field disambiguation? "compatible"|"earlier"|"later"|"reject"

---What a Span's calendar units are measured from. Give `relative`, or `days_are_24_hours`.
---@class datetime.SpanOptions
---@field relative? datetime.Date|datetime.DateTime|datetime.Zoned
---@field days_are_24_hours? boolean

---@class datetime.SpanRoundOptions: datetime.SpanOptions, datetime.DifferenceOptions

---The fields `with` takes on a Date, a DateTime or a Zoned.
---@class datetime.DateFields
---@field year? integer
---@field month? integer
---@field day? integer
---@field day_of_year? integer
---@field day_of_year_no_leap? integer

---The fields `with` takes on a Time, a DateTime or a Zoned.
---@class datetime.TimeFields
---@field hour? integer
---@field minute? integer
---@field second? integer
---@field millisecond? integer
---@field microsecond? integer
---@field nanosecond? integer
---@field subsec_nanosecond? integer

---@class datetime.DateTimeFields: datetime.DateFields, datetime.TimeFields
---@field date? datetime.Date
---@field time? datetime.Time

---@class datetime.ZonedFields: datetime.DateTimeFields
---@field disambiguation? "compatible"|"earlier"|"later"|"reject"

---The units `datetime.span` takes.
---@class datetime.SpanFields
---@field years? integer
---@field months? integer
---@field weeks? integer
---@field days? integer
---@field hours? integer
---@field minutes? integer
---@field seconds? integer
---@field milliseconds? integer
---@field microseconds? integer
---@field nanoseconds? integer

---The getters a Date, a DateTime and a Zoned share.
---@class datetime.DateParts
local DateParts = {}
---@return integer
function DateParts:year() end
---@return integer
function DateParts:month() end
---@return integer
function DateParts:day() end
---@return datetime.Weekday
function DateParts:weekday() end
---@return integer
function DateParts:day_of_year() end
---@return integer
function DateParts:day_of_year_no_leap() end
---@return integer
function DateParts:days_in_month() end
---@return integer
function DateParts:days_in_year() end
---@return boolean
function DateParts:in_leap_year() end

---The getters a Time, a DateTime and a Zoned share.
---@class datetime.TimeParts
local TimeParts = {}
---@return integer
function TimeParts:hour() end
---@return integer
function TimeParts:minute() end
---@return integer
function TimeParts:second() end
---@return integer
function TimeParts:millisecond() end
---@return integer
function TimeParts:microsecond() end
---@return integer
function TimeParts:nanosecond() end
---@return integer
function TimeParts:subsec_nanosecond() end

---An instant, with no time zone. `tostring` gives RFC 3339 in UTC.
---@class datetime.Timestamp
---@operator add(datetime.Duration): datetime.Timestamp
---@operator sub(datetime.Duration): datetime.Timestamp
---@operator sub(datetime.Timestamp): datetime.Span
local Timestamp = {}
---@param d datetime.Duration
---@return datetime.Timestamp
function Timestamp:add(d) end
---@param d datetime.Duration
---@return datetime.Timestamp
function Timestamp:sub(d) end
---@param other datetime.Timestamp
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function Timestamp:span_until(other, options) end
---@param other datetime.Timestamp
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function Timestamp:span_since(other, options) end
---@param other datetime.Timestamp
---@return datetime.SignedDuration
function Timestamp:duration_until(other) end
---@param other datetime.Timestamp
---@return datetime.SignedDuration
function Timestamp:duration_since(other) end
---@param options datetime.Unit|datetime.RoundOptions
---@return datetime.Timestamp
function Timestamp:round(options) end
---@param format string
---@return string
function Timestamp:strftime(format) end
---@return integer
function Timestamp:as_second() end
---@return integer
function Timestamp:as_millisecond() end
---@return integer
function Timestamp:as_microsecond() end
---Raises past the year 2262, where the count no longer fits in a Lua integer.
---@return integer
function Timestamp:as_nanosecond() end
---@return integer
function Timestamp:subsec_millisecond() end
---@return integer
function Timestamp:subsec_microsecond() end
---@return integer
function Timestamp:subsec_nanosecond() end
---@return integer
function Timestamp:signum() end
---@return boolean
function Timestamp:is_zero() end
---@return datetime.SignedDuration
function Timestamp:as_duration() end
---@param tz datetime.TimeZone
---@return datetime.Zoned
function Timestamp:to_zoned(tz) end
---@param name string
---@return datetime.Zoned
function Timestamp:in_tz(name) end

---An instant in a time zone. `tostring` gives RFC 9557, with the zone in brackets.
---@class datetime.Zoned: datetime.DateParts, datetime.TimeParts
---@operator add(datetime.Duration): datetime.Zoned
---@operator sub(datetime.Duration): datetime.Zoned
---@operator sub(datetime.Zoned): datetime.Span
local Zoned = {}
---@param d datetime.Duration
---@return datetime.Zoned
function Zoned:add(d) end
---@param d datetime.Duration
---@return datetime.Zoned
function Zoned:sub(d) end
---@param other datetime.Zoned
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function Zoned:span_until(other, options) end
---@param other datetime.Zoned
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function Zoned:span_since(other, options) end
---@param other datetime.Zoned
---@return datetime.SignedDuration
function Zoned:duration_until(other) end
---@param other datetime.Zoned
---@return datetime.SignedDuration
function Zoned:duration_since(other) end
---@param options datetime.Unit|datetime.RoundOptions
---@return datetime.Zoned
function Zoned:round(options) end
---@param format string
---@return string
function Zoned:strftime(format) end
---@param fields datetime.ZonedFields
---@return datetime.Zoned
function Zoned:with(fields) end
---@return datetime.Zoned
function Zoned:first_of_month() end
---@return datetime.Zoned
function Zoned:last_of_month() end
---@return datetime.Zoned
function Zoned:first_of_year() end
---@return datetime.Zoned
function Zoned:last_of_year() end
---@return datetime.Zoned
function Zoned:tomorrow() end
---@return datetime.Zoned
function Zoned:yesterday() end
---@param nth integer
---@param weekday datetime.WeekdayLike
---@return datetime.Zoned
function Zoned:nth_weekday(nth, weekday) end
---@param nth integer
---@param weekday datetime.WeekdayLike
---@return datetime.Zoned
function Zoned:nth_weekday_of_month(nth, weekday) end
---@return datetime.Zoned
function Zoned:start_of_day() end
---@return datetime.Zoned
function Zoned:end_of_day() end
---@return datetime.Timestamp
function Zoned:timestamp() end
---@return datetime.DateTime
function Zoned:datetime() end
---@return datetime.Date
function Zoned:date() end
---@return datetime.Time
function Zoned:time() end
---@return datetime.TimeZone
function Zoned:time_zone() end
---The same instant in another zone.
---@param tz datetime.TimeZone
---@return datetime.Zoned
function Zoned:with_time_zone(tz) end
---The same instant in the zone named.
---@param name string
---@return datetime.Zoned
function Zoned:in_tz(name) end

---A calendar date. `tostring` gives `2024-01-02`.
---@class datetime.Date: datetime.DateParts
---@operator add(datetime.Duration): datetime.Date
---@operator sub(datetime.Duration): datetime.Date
---@operator sub(datetime.Date): datetime.Span
local Date = {}
---@param d datetime.Duration
---@return datetime.Date
function Date:add(d) end
---@param d datetime.Duration
---@return datetime.Date
function Date:sub(d) end
---@param other datetime.Date
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function Date:span_until(other, options) end
---@param other datetime.Date
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function Date:span_since(other, options) end
---@param other datetime.Date
---@return datetime.SignedDuration
function Date:duration_until(other) end
---@param other datetime.Date
---@return datetime.SignedDuration
function Date:duration_since(other) end
---@param format string
---@return string
function Date:strftime(format) end
---@param fields datetime.DateFields
---@return datetime.Date
function Date:with(fields) end
---@return datetime.Date
function Date:first_of_month() end
---@return datetime.Date
function Date:last_of_month() end
---@return datetime.Date
function Date:first_of_year() end
---@return datetime.Date
function Date:last_of_year() end
---@return datetime.Date
function Date:tomorrow() end
---@return datetime.Date
function Date:yesterday() end
---@param nth integer
---@param weekday datetime.WeekdayLike
---@return datetime.Date
function Date:nth_weekday(nth, weekday) end
---@param nth integer
---@param weekday datetime.WeekdayLike
---@return datetime.Date
function Date:nth_weekday_of_month(nth, weekday) end
---@param hour integer
---@param minute integer
---@param second? integer
---@param nanosecond? integer
---@return datetime.DateTime
function Date:at(hour, minute, second, nanosecond) end
---@param time datetime.Time
---@return datetime.DateTime
function Date:to_datetime(time) end
---Midnight on this date, in `tz`.
---@param tz datetime.TimeZone
---@param options? datetime.ZonedOptions
---@return datetime.Zoned
function Date:to_zoned(tz, options) end
---Midnight on this date, in the zone named.
---@param name string
---@param options? datetime.ZonedOptions
---@return datetime.Zoned
function Date:in_tz(name, options) end

---A time of day. `tostring` gives `03:04:05`.
---@class datetime.Time: datetime.TimeParts
---@operator add(datetime.Duration): datetime.Time
---@operator sub(datetime.Duration): datetime.Time
---@operator sub(datetime.Time): datetime.Span
local Time = {}
---@param d datetime.Duration
---@return datetime.Time
function Time:add(d) end
---@param d datetime.Duration
---@return datetime.Time
function Time:sub(d) end
---@param other datetime.Time
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function Time:span_until(other, options) end
---@param other datetime.Time
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function Time:span_since(other, options) end
---@param other datetime.Time
---@return datetime.SignedDuration
function Time:duration_until(other) end
---@param other datetime.Time
---@return datetime.SignedDuration
function Time:duration_since(other) end
---@param options datetime.Unit|datetime.RoundOptions
---@return datetime.Time
function Time:round(options) end
---@param format string
---@return string
function Time:strftime(format) end
---@param fields datetime.TimeFields
---@return datetime.Time
function Time:with(fields) end
---@param year integer
---@param month integer
---@param day integer
---@return datetime.DateTime
function Time:on(year, month, day) end
---@param date datetime.Date
---@return datetime.DateTime
function Time:to_datetime(date) end

---A date and a time of day, with no time zone. `tostring` gives `2024-01-02T03:04:05`.
---@class datetime.DateTime: datetime.DateParts, datetime.TimeParts
---@operator add(datetime.Duration): datetime.DateTime
---@operator sub(datetime.Duration): datetime.DateTime
---@operator sub(datetime.DateTime): datetime.Span
local DateTime = {}
---@param d datetime.Duration
---@return datetime.DateTime
function DateTime:add(d) end
---@param d datetime.Duration
---@return datetime.DateTime
function DateTime:sub(d) end
---@param other datetime.DateTime
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function DateTime:span_until(other, options) end
---@param other datetime.DateTime
---@param options? datetime.DifferenceOptions
---@return datetime.Span
function DateTime:span_since(other, options) end
---@param other datetime.DateTime
---@return datetime.SignedDuration
function DateTime:duration_until(other) end
---@param other datetime.DateTime
---@return datetime.SignedDuration
function DateTime:duration_since(other) end
---@param options datetime.Unit|datetime.RoundOptions
---@return datetime.DateTime
function DateTime:round(options) end
---@param format string
---@return string
function DateTime:strftime(format) end
---@param fields datetime.DateTimeFields
---@return datetime.DateTime
function DateTime:with(fields) end
---@return datetime.DateTime
function DateTime:first_of_month() end
---@return datetime.DateTime
function DateTime:last_of_month() end
---@return datetime.DateTime
function DateTime:first_of_year() end
---@return datetime.DateTime
function DateTime:last_of_year() end
---@return datetime.DateTime
function DateTime:tomorrow() end
---@return datetime.DateTime
function DateTime:yesterday() end
---@param nth integer
---@param weekday datetime.WeekdayLike
---@return datetime.DateTime
function DateTime:nth_weekday(nth, weekday) end
---@param nth integer
---@param weekday datetime.WeekdayLike
---@return datetime.DateTime
function DateTime:nth_weekday_of_month(nth, weekday) end
---@return datetime.DateTime
function DateTime:start_of_day() end
---@return datetime.DateTime
function DateTime:end_of_day() end
---@return datetime.Date
function DateTime:date() end
---@return datetime.Time
function DateTime:time() end
---@param tz datetime.TimeZone
---@param options? datetime.ZonedOptions
---@return datetime.Zoned
function DateTime:to_zoned(tz, options) end
---@param name string
---@param options? datetime.ZonedOptions
---@return datetime.Zoned
function DateTime:in_tz(name, options) end

---A span of time in calendar and clock units, each kept apart: `{ hours = 1 }` and
---`{ minutes = 60 }` are not `==`, though `compare` says they are as long. `tostring` gives ISO 8601
---(`P5DT3H`). `+`, `<` and `<=` raise, because a day or a month has no one length: use `add` and
---`compare`, which take a `relative` date to measure from.
---@class datetime.Span
---@operator unm: datetime.Span
---@operator mul(integer): datetime.Span
local Span = {}
---@return integer
function Span:get_years() end
---@return integer
function Span:get_months() end
---@return integer
function Span:get_weeks() end
---@return integer
function Span:get_days() end
---@return integer
function Span:get_hours() end
---@return integer
function Span:get_minutes() end
---@return integer
function Span:get_seconds() end
---@return integer
function Span:get_milliseconds() end
---@return integer
function Span:get_microseconds() end
---@return integer
function Span:get_nanoseconds() end
---A copy with this many years.
---@param n integer
---@return datetime.Span
function Span:years(n) end
---@param n integer
---@return datetime.Span
function Span:months(n) end
---@param n integer
---@return datetime.Span
function Span:weeks(n) end
---@param n integer
---@return datetime.Span
function Span:days(n) end
---@param n integer
---@return datetime.Span
function Span:hours(n) end
---@param n integer
---@return datetime.Span
function Span:minutes(n) end
---@param n integer
---@return datetime.Span
function Span:seconds(n) end
---@param n integer
---@return datetime.Span
function Span:milliseconds(n) end
---@param n integer
---@return datetime.Span
function Span:microseconds(n) end
---@param n integer
---@return datetime.Span
function Span:nanoseconds(n) end
---@return datetime.Span
function Span:abs() end
---@return datetime.Span
function Span:negate() end
---@return integer
function Span:signum() end
---@return boolean
function Span:is_zero() end
---@return boolean
function Span:is_negative() end
---@return boolean
function Span:is_positive() end
---@param n integer
---@return datetime.Span
function Span:mul(n) end
---@param other datetime.Duration
---@param options? datetime.SpanOptions
---@return datetime.Span
function Span:add(other, options) end
---@param other datetime.Duration
---@param options? datetime.SpanOptions
---@return datetime.Span
function Span:sub(other, options) end
---`-1`, `0` or `1`.
---@param other datetime.Duration
---@param options? datetime.SpanOptions
---@return integer
function Span:compare(other, options) end
---How many of `unit` this span is.
---@param unit datetime.Unit
---@param options? datetime.SpanOptions
---@return number
function Span:total(unit, options) end
---@param options datetime.Unit|datetime.SpanRoundOptions
---@return datetime.Span
function Span:round(options) end
---@param options? datetime.SpanOptions
---@return datetime.SignedDuration
function Span:to_duration(options) end

---An exact length of time, in seconds and nanoseconds. `tostring` gives ISO 8601 (`PT1M30S`).
---@class datetime.SignedDuration
---@operator add(datetime.SignedDuration): datetime.SignedDuration
---@operator sub(datetime.SignedDuration): datetime.SignedDuration
---@operator unm: datetime.SignedDuration
---@operator mul(integer): datetime.SignedDuration
---@operator div(integer): datetime.SignedDuration
local SignedDuration = {}
---@param other datetime.SignedDuration
---@return datetime.SignedDuration
function SignedDuration:add(other) end
---@param other datetime.SignedDuration
---@return datetime.SignedDuration
function SignedDuration:sub(other) end
---@param n integer
---@return datetime.SignedDuration
function SignedDuration:mul(n) end
---@param n integer
---@return datetime.SignedDuration
function SignedDuration:div(n) end
---@return datetime.SignedDuration
function SignedDuration:neg() end
---@return datetime.SignedDuration
function SignedDuration:abs() end
---@return integer
function SignedDuration:as_secs() end
---@return integer
function SignedDuration:as_mins() end
---@return integer
function SignedDuration:as_hours() end
---@return integer
function SignedDuration:as_millis() end
---@return integer
function SignedDuration:as_micros() end
---@return integer
function SignedDuration:as_nanos() end
---@return number
function SignedDuration:as_secs_f64() end
---@return number
function SignedDuration:as_millis_f64() end
---@return integer
function SignedDuration:subsec_millis() end
---@return integer
function SignedDuration:subsec_micros() end
---@return integer
function SignedDuration:subsec_nanos() end
---@return boolean
function SignedDuration:is_zero() end
---@return boolean
function SignedDuration:is_negative() end
---@return boolean
function SignedDuration:is_positive() end
---@return integer
function SignedDuration:signum() end
---@param options datetime.Unit|datetime.RoundOptions
---@return datetime.SignedDuration
function SignedDuration:round(options) end

---The rules for turning an instant into a civil time. `tostring` gives its IANA name.
---@class datetime.TimeZone
local TimeZone = {}
---@return string?
function TimeZone:iana_name() end
---@param dt datetime.DateTime
---@param options? datetime.ZonedOptions
---@return datetime.Zoned
function TimeZone:to_zoned(dt, options) end
---@param ts datetime.Timestamp
---@return datetime.DateTime
function TimeZone:to_datetime(ts) end

---A day of the week. `tostring` gives its name, `"Monday"`.
---@class datetime.Weekday
local Weekday = {}
---@return integer
function Weekday:to_monday_zero_offset() end
---@return integer
function Weekday:to_monday_one_offset() end
---@return integer
function Weekday:to_sunday_zero_offset() end
---@return integer
function Weekday:to_sunday_one_offset() end
---@return datetime.Weekday
function Weekday:next() end
---@return datetime.Weekday
function Weekday:previous() end
---Days forward from this weekday to `other`, 0 to 6.
---@param other datetime.WeekdayLike
---@return integer
function Weekday:days_until(other) end
---Days back from this weekday to `other`, 0 to 6.
---@param other datetime.WeekdayLike
---@return integer
function Weekday:days_since(other) end
---@param n integer
---@return datetime.Weekday
function Weekday:wrapping_add(n) end
---@param n integer
---@return datetime.Weekday
function Weekday:wrapping_sub(n) end

---@class datetime
local datetime = {}

---@class datetime.TimestampTable
datetime.Timestamp = {}
---@return datetime.Timestamp
function datetime.Timestamp.now() end
---@param second integer
---@param nanosecond? integer
---@return datetime.Timestamp
function datetime.Timestamp.new(second, nanosecond) end
---@param n integer
---@return datetime.Timestamp
function datetime.Timestamp.from_second(n) end
---@param n integer
---@return datetime.Timestamp
function datetime.Timestamp.from_millisecond(n) end
---@param n integer
---@return datetime.Timestamp
function datetime.Timestamp.from_microsecond(n) end
---@param n integer
---@return datetime.Timestamp
function datetime.Timestamp.from_nanosecond(n) end
---@param d datetime.SignedDuration
---@return datetime.Timestamp
function datetime.Timestamp.from_duration(d) end
---@param s string
---@return datetime.Timestamp
function datetime.Timestamp.parse(s) end
---@param format string
---@param s string
---@return datetime.Timestamp
function datetime.Timestamp.strptime(format, s) end

---@class datetime.ZonedTable
datetime.Zoned = {}
---Now, in the system's time zone, or in UTC if it cannot be found.
---@return datetime.Zoned
function datetime.Zoned.now() end
---@param ts datetime.Timestamp
---@param tz datetime.TimeZone
---@return datetime.Zoned
function datetime.Zoned.new(ts, tz) end
---@param s string
---@return datetime.Zoned
function datetime.Zoned.parse(s) end
---@param format string
---@param s string
---@return datetime.Zoned
function datetime.Zoned.strptime(format, s) end

---@class datetime.DateTable
datetime.Date = {}
---@param year integer
---@param month integer
---@param day integer
---@return datetime.Date
function datetime.Date.new(year, month, day) end
---@param s string
---@return datetime.Date
function datetime.Date.parse(s) end
---@param format string
---@param s string
---@return datetime.Date
function datetime.Date.strptime(format, s) end

---@class datetime.TimeTable
datetime.Time = {}
---@param hour integer
---@param minute integer
---@param second? integer
---@param nanosecond? integer
---@return datetime.Time
function datetime.Time.new(hour, minute, second, nanosecond) end
---@return datetime.Time
function datetime.Time.midnight() end
---@param s string
---@return datetime.Time
function datetime.Time.parse(s) end
---@param format string
---@param s string
---@return datetime.Time
function datetime.Time.strptime(format, s) end

---@class datetime.DateTimeTable
datetime.DateTime = {}
---@param year integer
---@param month integer
---@param day integer
---@param hour? integer
---@param minute? integer
---@param second? integer
---@param nanosecond? integer
---@return datetime.DateTime
function datetime.DateTime.new(year, month, day, hour, minute, second, nanosecond) end
---@param s string
---@return datetime.DateTime
function datetime.DateTime.parse(s) end
---@param format string
---@param s string
---@return datetime.DateTime
function datetime.DateTime.strptime(format, s) end

---@class datetime.SpanTable
datetime.Span = {}
---The empty span.
---@return datetime.Span
function datetime.Span.new() end
---ISO 8601 (`P5DT3H`) or the friendly format (`5 days 3 hours`).
---@param s string
---@return datetime.Span
function datetime.Span.parse(s) end

---@class datetime.SignedDurationTable
---@field ZERO datetime.SignedDuration
datetime.SignedDuration = {}
---@param secs integer
---@param nanos? integer
---@return datetime.SignedDuration
function datetime.SignedDuration.new(secs, nanos) end
---@param n integer
---@return datetime.SignedDuration
function datetime.SignedDuration.from_secs(n) end
---@param n integer
---@return datetime.SignedDuration
function datetime.SignedDuration.from_millis(n) end
---@param n integer
---@return datetime.SignedDuration
function datetime.SignedDuration.from_micros(n) end
---@param n integer
---@return datetime.SignedDuration
function datetime.SignedDuration.from_nanos(n) end
---@param n integer
---@return datetime.SignedDuration
function datetime.SignedDuration.from_mins(n) end
---@param n integer
---@return datetime.SignedDuration
function datetime.SignedDuration.from_hours(n) end
---@param n number
---@return datetime.SignedDuration
function datetime.SignedDuration.from_secs_f64(n) end
---ISO 8601 (`PT1M30S`) or the friendly format (`1m 30s`).
---@param s string
---@return datetime.SignedDuration
function datetime.SignedDuration.parse(s) end

---@class datetime.TimeZoneTable
---@field UTC datetime.TimeZone
datetime.TimeZone = {}
---A zone by its IANA name, such as `"Europe/London"`.
---@param name string
---@return datetime.TimeZone
function datetime.TimeZone.get(name) end
---The system's zone, or UTC if it cannot be found.
---@return datetime.TimeZone
function datetime.TimeZone.system() end
---The system's zone, raising if it cannot be found.
---@return datetime.TimeZone
function datetime.TimeZone.try_system() end

---@class datetime.WeekdayTable
---@field monday datetime.Weekday
---@field tuesday datetime.Weekday
---@field wednesday datetime.Weekday
---@field thursday datetime.Weekday
---@field friday datetime.Weekday
---@field saturday datetime.Weekday
---@field sunday datetime.Weekday
datetime.Weekday = {}
---A weekday by its English name, full or three letters, in any case.
---@param name string
---@return datetime.Weekday
function datetime.Weekday.from_name(name) end
---@param n integer 0 to 6, Monday first.
---@return datetime.Weekday
function datetime.Weekday.from_monday_zero_offset(n) end
---@param n integer 1 to 7, Monday first.
---@return datetime.Weekday
function datetime.Weekday.from_monday_one_offset(n) end
---@param n integer 0 to 6, Sunday first.
---@return datetime.Weekday
function datetime.Weekday.from_sunday_zero_offset(n) end
---@param n integer 1 to 7, Sunday first.
---@return datetime.Weekday
function datetime.Weekday.from_sunday_one_offset(n) end

---`Date.new`.
---@param year integer
---@param month integer
---@param day integer
---@return datetime.Date
function datetime.date(year, month, day) end

---`Time.new`.
---@param hour integer
---@param minute integer
---@param second? integer
---@param nanosecond? integer
---@return datetime.Time
function datetime.time(hour, minute, second, nanosecond) end

---`DateTime.new`.
---@param year integer
---@param month integer
---@param day integer
---@param hour? integer
---@param minute? integer
---@param second? integer
---@param nanosecond? integer
---@return datetime.DateTime
function datetime.datetime(year, month, day, hour, minute, second, nanosecond) end

---A span with the units given. An unknown key raises.
---@param fields datetime.SpanFields
---@return datetime.Span
function datetime.span(fields) end

---Waits. `amount` is a SignedDuration, a Span in hours or smaller, or a whole number of
---milliseconds. A negative amount raises.
---@async
---@param amount datetime.SignedDuration|datetime.Span|integer
function datetime.sleep(amount) end

-- Everything above is for LuaLS. What runs is this: the Rust half hands the module over as a value
-- (see `components/datetime.rs`), and this file returns it.
return ...
