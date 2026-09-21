//! Which stdlib modules a build has: the modules whose Cargo feature is on, and no others.
//!
//! The expectation comes from the features this test was itself built with, so the same file
//! passes under every combination `scripts/check-features.sh` builds, and means something under
//! each.

use avarice_rt_stdlib::{StdModule, StdModules, loader};

/// Each module's name, and whether the feature that compiles it in is on in this build.
const EXPECTED: [(&str, bool); 8] = [
    ("http", cfg!(feature = "stdlib-http")),
    ("fs", cfg!(feature = "stdlib-fs")),
    ("crypto", cfg!(feature = "stdlib-crypto")),
    ("serde", cfg!(feature = "stdlib-serde")),
    ("datetime", cfg!(feature = "stdlib-datetime")),
    ("utils", cfg!(feature = "stdlib-utils")),
    ("stores", cfg!(feature = "stdlib-stores")),
    ("validation", cfg!(feature = "stdlib-validation")),
];

/// Every module, whether or not it is compiled in: the type has all eight variants in every build.
fn every_module() -> Vec<StdModule> {
    StdModules::all().modules().collect()
}

fn feature_is_on(module: StdModule) -> bool {
    EXPECTED
        .iter()
        .find(|(name, _)| *name == module.name())
        .map(|(_, on)| *on)
        .expect("every module is in EXPECTED")
}

#[test]
fn every_module_exists_in_every_build() {
    let names: Vec<_> = every_module().into_iter().map(StdModule::name).collect();
    assert_eq!(names, EXPECTED.map(|(name, _)| name));
}

#[test]
fn all_is_exactly_the_modules_whose_feature_is_on() {
    let compiled_in: Vec<_> = StdModules::ALL.modules().map(StdModule::name).collect();
    let expected: Vec<_> = EXPECTED
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name)
        .collect();
    assert_eq!(compiled_in, expected);
}

#[test]
fn the_module_list_is_the_same_modules_in_the_same_order() {
    let listed: Vec<_> = StdModule::ALL
        .iter()
        .copied()
        .map(StdModule::name)
        .collect();
    let set: Vec<_> = StdModules::ALL.modules().map(StdModule::name).collect();
    assert_eq!(listed, set);
}

#[test]
fn a_module_knows_whether_it_is_compiled_in() {
    for module in every_module() {
        assert_eq!(module.is_compiled_in(), feature_is_on(module), "{module:?}");
    }
}

#[test]
fn what_is_not_compiled_in_is_what_all_leaves_out() {
    let missing: Vec<_> = StdModules::all()
        .not_compiled_in()
        .modules()
        .map(StdModule::name)
        .collect();
    let expected: Vec<_> = EXPECTED
        .into_iter()
        .filter(|(_, on)| !*on)
        .map(|(name, _)| name)
        .collect();
    assert_eq!(missing, expected);
}

#[test]
fn a_module_names_the_feature_that_compiles_it_in() {
    for module in every_module() {
        assert_eq!(module.feature(), format!("stdlib-{}", module.name()));
    }
}

#[test]
fn loading_a_module_that_is_not_compiled_in_says_which_feature_is_missing() {
    let lua = mlua::Lua::new();
    for module in every_module() {
        if module.is_compiled_in() {
            continue;
        }
        let error = loader(module)(&lua).expect_err("not compiled in, so it cannot be built");
        assert!(
            error.to_string().contains(module.feature()),
            "{module:?}: {error}"
        );
    }
}

#[test]
fn a_selection_is_refused_when_it_names_a_module_that_is_not_compiled_in() {
    assert!(StdModules::ALL.require_compiled_in().is_ok());
    assert!(StdModules::NONE.require_compiled_in().is_ok());

    let missing: Vec<_> = EXPECTED
        .into_iter()
        .filter(|(_, on)| !*on)
        .map(|(name, _)| name)
        .collect();
    match StdModules::all().require_compiled_in() {
        Ok(()) => assert!(missing.is_empty(), "{missing:?} are not compiled in"),
        Err(error) => {
            let message = error.to_string();
            assert!(!missing.is_empty(), "nothing is missing, yet: {message}");
            for module in every_module() {
                assert_eq!(
                    message.contains(module.feature()),
                    missing.contains(&module.name()),
                    "{}: {message}",
                    module.feature()
                );
            }
        }
    }
}
