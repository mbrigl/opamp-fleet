//! The kinds of change one Server instance tells the others of
//! (ADR-0045
//! clause 9). A store that shares its state among instances announces each change it commits as
//! one of these, with the identifier it concerns; an instance that hears one maps it once to what
//! it reads again. How a kind travels is the adapter's: this type names none of it.
//!
//! The variants and [`Changed::ALL`] come from one list, so a kind added is one variant there, and
//! every `match` that maps a kind fails to compile until it maps the new one.

/// Declares [`Changed`] and [`Changed::ALL`] from one list of variants.
macro_rules! changes {
    ($($(#[$doc:meta])* $kind:ident,)+) => {
        /// A kind of change one Server instance tells the others of.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Changed {
            $($(#[$doc])* $kind,)+
        }

        impl Changed {
            /// Every kind, in the order of their declaration.
            pub const ALL: &'static [Changed] = &[$(Changed::$kind,)+];
        }
    };
}

changes! {
    /// An Agent's assignment changed by a rollout, by its `instance_uid`.
    Assignment,
    /// A Configuration saved, collected or deleted, by its name.
    Configuration,
    /// A Deployment written or deleted, or a member of it added or removed, by its name.
    Deployment,
    /// A Package created or deleted, or an entry or a signature of it written or removed, by its
    /// `<agent type>@<version>`.
    Package,
    /// A restart requested for an Agent, by its `instance_uid`.
    Restart,
    /// A revocation added or lifted.
    Revocation,
    /// A register entry written or removed, by its serial.
    Certificate,
    /// A Host written or removed, or an `instance_uid` bound to it or released, by the Host where
    /// a binding changed.
    Host,
    /// An enrolment request queued, decided or withdrawn.
    Enrolment,
    /// The enrolment window opened, moved, closed or ended.
    Window,
    /// A package floor set or removed, by its Agent type.
    Floor,
    /// An Agent's labels set, by its `instance_uid`.
    Labels,
    /// An Agent forgotten, by its `instance_uid`.
    Forgotten,
    /// An Agent taken from another instance, by its `instance_uid`: the instance it was taken
    /// from reads its holder again and stops carrying it (clause 8).
    Taken,
    /// A Client Agent first arrived over a Gateway's Host, by that Host: the other instances place
    /// the Gateway's groups again, so a fellow Agent they hold finds it (clause 8).
    Arrived,
}
