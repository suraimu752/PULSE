//! How to describe processes that could not be opened.
//!
//! `ERROR_ACCESS_DENIED` while already elevated is a protected process
//! (System, Secure System, Registry, csrss, smss, MemCompression, and similar).
//! Another elevation will not open it. The same error while not elevated may
//! be a missing privilege, and those two cases are not the same sentence.
//! Any other open failure is "could not open", not "needs admin".

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessBucket {
    NeedsAdmin,
    Protected,
    Other,
}

/// `access_denied` is `ERROR_ACCESS_DENIED` from `OpenProcess`.
/// While elevated, that error is protected, not a hint to elevate again.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn bucket(elevated: bool, access_denied: bool) -> AccessBucket {
    match (elevated, access_denied) {
        (_, false) => AccessBucket::Other,
        (false, true) => AccessBucket::NeedsAdmin,
        (true, true) => AccessBucket::Protected,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessCounts {
    pub needs_admin: u32,
    pub protected: u32,
    pub other: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessNote {
    pub text: String,
    /// Quiet notes are not an amber warning. Nothing the user can click will
    /// open a protected process.
    pub quiet: bool,
}

pub fn access_warning(counts: AccessCounts) -> Option<AccessNote> {
    let AccessCounts {
        needs_admin,
        protected,
        other,
    } = counts;
    if needs_admin == 0 && protected == 0 && other == 0 {
        return None;
    }
    if needs_admin > 0 {
        let text = if protected == 0 && other == 0 {
            format!("{needs_admin} 件は権限不足のため CPU またはメモリを計測できませんでした")
        } else if protected == 0 {
            format!("{needs_admin} 件は権限不足、{other} 件は開けませんでした")
        } else if other == 0 {
            format!("{needs_admin} 件は権限不足、保護された {protected} 件は読めません")
        } else {
            format!(
                "{needs_admin} 件は権限不足、保護 {protected} 件とその他 {other} 件は読めません"
            )
        };
        return Some(AccessNote { text, quiet: false });
    }
    let text = if other == 0 {
        format!("{protected} 件は保護されていて読めません")
    } else if protected == 0 {
        format!("{other} 件は開けず、CPU またはメモリを読めません")
    } else {
        format!("保護 {protected} 件とその他 {other} 件は読めません")
    };
    Some(AccessNote { text, quiet: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevated_access_denied_is_protected_not_needs_admin() {
        assert_eq!(bucket(true, true), AccessBucket::Protected);
        assert_eq!(bucket(false, true), AccessBucket::NeedsAdmin);
        assert_eq!(bucket(true, false), AccessBucket::Other);
        assert_eq!(bucket(false, false), AccessBucket::Other);
    }

    #[test]
    fn protected_note_does_not_ask_for_elevation() {
        let note = access_warning(AccessCounts {
            needs_admin: 0,
            protected: 12,
            other: 0,
        })
        .expect("note");
        assert!(note.quiet);
        assert!(note.text.contains("12"));
        assert!(note.text.contains("保護"));
        assert!(!note.text.contains("権限不足"));
        assert!(!note.text.contains("管理者"));
    }

    #[test]
    fn needs_admin_stays_a_warning_and_zero_is_silent() {
        let note = access_warning(AccessCounts {
            needs_admin: 3,
            protected: 0,
            other: 0,
        })
        .expect("note");
        assert!(!note.quiet);
        assert!(note.text.contains("権限不足"));
        assert!(access_warning(AccessCounts {
            needs_admin: 0,
            protected: 0,
            other: 0,
        })
        .is_none());
    }

    #[test]
    fn mixed_failures_keep_separate_counts() {
        let elevated = access_warning(AccessCounts {
            needs_admin: 0,
            protected: 10,
            other: 2,
        })
        .expect("note");
        assert!(elevated.quiet);
        assert!(elevated.text.contains("10"));
        assert!(elevated.text.contains('2'));
        assert!(!elevated.text.contains("権限不足"));
        let mixed = access_warning(AccessCounts {
            needs_admin: 4,
            protected: 0,
            other: 1,
        })
        .expect("note");
        assert!(!mixed.quiet);
        assert!(mixed.text.contains('4'));
        assert!(mixed.text.contains('1'));
    }
}
