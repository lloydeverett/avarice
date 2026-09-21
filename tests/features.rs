//! A build with some stdlib modules left out (ADR 0007): what a runtime registers, and what
//! asking for a module that is not there does.
//!
//! Every test here holds under every combination `scripts/check-features.sh` builds, and each
//! means something only where some module is missing, or, for the ones about the modules that are
//! there, where some module is present. Which is which is decided by the build, never by the test:
//! the expectations come from `StdModule::ALL` and `StdModules::not_compiled_in`, the public
//! answer to "what is compiled in", not from a second list of features kept here.

mod common;

use avarice_rt::{Error, Profile, Runtime, StdModule, StdModules};
use common::{compiled_in, listed, requirable};

/// The modules this build does not have, one at a time.
fn missing() -> Vec<StdModule> {
    StdModules::all().not_compiled_in().modules().collect()
}

// -- Asking for a module that is not compiled in -----------------------------------------------

#[test]
fn asking_for_a_module_that_is_not_compiled_in_is_an_error_naming_its_feature() {
    for module in missing() {
        let error = Runtime::builder(Profile::Sandbox)
            .with_std_modules(module.into())
            .build()
            .err()
            .unwrap_or_else(|| panic!("{module:?} is not compiled in, yet build succeeded"));
        let Error::Config(message) = &error else {
            panic!("{module:?}: expected a configuration error, got {error:?}");
        };
        assert!(message.contains(module.feature()), "{message}");
        assert!(message.contains(module.name()), "{message}");
    }
}

#[test]
fn the_error_names_every_module_that_is_missing_and_no_other() {
    let missing = missing();
    if missing.is_empty() {
        return;
    }
    let Err(error) = Runtime::builder(Profile::Sandbox)
        .std_modules(StdModules::all())
        .build()
    else {
        panic!("some modules are not compiled in, so the full set is refused");
    };
    let message = error.to_string();
    for module in StdModules::all().modules() {
        assert_eq!(
            message.contains(module.feature()),
            missing.contains(&module),
            "{}: {message}",
            module.feature()
        );
    }
}

#[test]
fn taking_away_a_module_that_is_not_compiled_in_asks_for_less_and_is_not_an_error() {
    let rt = Runtime::builder(Profile::Trusted)
        .without_std_modules(StdModules::all())
        .build()
        .expect("taking modules away never asks for one that is not there");
    assert!(listed(&rt).is_empty());
}

#[test]
fn a_sandbox_asks_for_no_module_and_so_builds_in_every_build() {
    Runtime::new(Profile::Sandbox).unwrap();
}

// -- What the profiles register ----------------------------------------------------------------

#[test]
fn trusted_mode_registers_exactly_the_modules_that_are_compiled_in() {
    let rt = Runtime::new(Profile::Trusted).expect("trusted mode builds with any set compiled in");
    assert_eq!(listed(&rt), compiled_in());
    for module in StdModules::all().modules() {
        assert_eq!(
            rt.has_module(module.name()),
            module.is_compiled_in(),
            "{module:?}"
        );
        assert_eq!(
            requirable(&rt, module.name()),
            module.is_compiled_in(),
            "{module:?}"
        );
    }
}

#[test]
fn sandbox_mode_reaches_no_module_however_many_are_compiled_in() {
    let rt = Runtime::new(Profile::Sandbox).unwrap();
    assert!(listed(&rt).is_empty());
    for module in StdModules::all().modules() {
        assert!(!rt.has_module(module.name()), "{module:?}");
        assert!(!requirable(&rt, module.name()), "{module:?}");
    }
}
