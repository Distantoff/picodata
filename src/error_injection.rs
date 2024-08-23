use std::{collections::HashSet, env, ptr};

static mut INJECTED_ERRORS: Option<HashSet<String>> = None;

#[inline(always)]
pub fn enable(error: &str, enable: bool) {
    // SAFETY: safe as long as only called from tx thread
    let injected_errors = unsafe { ptr::addr_of_mut!(INJECTED_ERRORS).as_mut() }
        .unwrap()
        .get_or_insert_with(Default::default);

    // DO NOT LOG "ERROR INJECTION '{error}'" HERE!!!
    // We check this error message in tests to catch the moment the injected error happens
    if enable {
        injected_errors.insert(error.into());
    } else {
        injected_errors.remove(error);
    }
}

#[inline(always)]
pub fn is_enabled(error: &str) -> bool {
    // SAFETY: safe as long as only called from tx thread
    match unsafe { ptr::addr_of!(INJECTED_ERRORS).as_ref() }.unwrap() {
        Some(injected_errors) => injected_errors.contains(error),
        None => false,
    }
}

// Scan environment variables for `PICODATA_ERROR_INJECTION_<NAME>`-like
// patterns. Arm corresponding injection for each one.
pub fn set_from_env() {
    for (key, _) in env::vars() {
        let inject_name = match key.strip_prefix("PICODATA_ERROR_INJECTION_") {
            Some(name) => name,
            None => continue,
        };

        enable(inject_name, true)
    }
}

#[cfg(not(feature = "error_injection"))]
#[macro_export]
macro_rules! error_injection {
    ($($t:tt)*) => {
        // Error injection is disabled
    };
}

#[cfg(feature = "error_injection")]
#[macro_export]
macro_rules! error_injection {
    (exit $error:expr) => {
        #[rustfmt::skip]
        if $crate::error_injection::is_enabled($error) {
            $crate::tlog!(Info, "################################################################");
            $crate::tlog!(Info, "ERROR INJECTION '{}': EXITING", $error);
            $crate::tlog!(Info, "################################################################");
            $crate::tarantool::exit(69);
        };
    };
    (block $error:expr) => {{
        let error = $error;
        #[rustfmt::skip]
        if $crate::error_injection::is_enabled(error) {
            $crate::tlog!(Info, "################################################################");
            $crate::tlog!(Info, "ERROR INJECTION '{}': BLOCKING", error);
            $crate::tlog!(Info, "################################################################");

            while $crate::error_injection::is_enabled(error) {
                ::tarantool::fiber::sleep(::std::time::Duration::from_millis(100));
            }

            $crate::tlog!(Info, "################################################################");
            $crate::tlog!(Info, "ERROR INJECTION '{}': UNBLOCKING", error);
            $crate::tlog!(Info, "################################################################");
        };
    }};
    ($error:expr => return $result:expr) => {{
        let error = $error;
        if $crate::error_injection::is_enabled(error) {
            let result = $result;
            $crate::tlog!(Info, "################################################################");
            $crate::tlog!(Info, "ERROR INJECTION '{error}': RETURNING {result:?}");
            $crate::tlog!(Info, "################################################################");

            return result;
        }
    }}
}
