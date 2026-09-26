//! Forward-compatible string enums.
//!
//! Databricks adds enum values frequently. A closed serde enum would fail to
//! deserialise the whole response when a new value appears, so every enum in
//! the SDK carries an `Unknown(String)` variant that preserves the raw value
//! and round-trips it unchanged.

/// Declares a forward-compatible string enum.
///
/// ```
/// community_databricks_core::open_enum! {
///     /// Cluster state.
///     pub enum State {
///         /// Pending.
///         Pending => "PENDING",
///         /// Running.
///         Running => "RUNNING",
///     }
/// }
/// assert_eq!(State::from("RUNNING"), State::Running);
/// assert_eq!(State::from("NEW_ONE"), State::Unknown("NEW_ONE".into()));
/// assert_eq!(State::Unknown("NEW_ONE".into()).as_str(), "NEW_ONE");
/// ```
#[macro_export]
macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this version of the SDK does not know about yet.
            Unknown(String),
        }

        impl $name {
            /// The wire representation of this value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $wire, )+
                    Self::Unknown(s) => s.as_str(),
                }
            }

            /// Every value known to this version of the SDK.
            pub const KNOWN: &'static [&'static str] = &[$( $wire ),+];
        }

        // An empty `Unknown` value, used when a required field is absent
        // from a response.
        impl ::core::default::Default for $name {
            fn default() -> Self {
                Self::Unknown(::std::string::String::new())
            }
        }

        impl ::core::convert::From<&str> for $name {
            fn from(s: &str) -> Self {
                match s {
                    $( $wire => Self::$variant, )+
                    other => Self::Unknown(other.to_owned()),
                }
            }
        }

        impl ::core::convert::From<String> for $name {
            fn from(s: String) -> Self {
                Self::from(s.as_str())
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl $crate::__private::serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> ::core::result::Result<S::Ok, S::Error>
            where
                S: $crate::__private::serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> $crate::__private::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>
            where
                D: $crate::__private::serde::Deserializer<'de>,
            {
                let s = <::std::string::String as $crate::__private::serde::Deserialize>::deserialize(deserializer)?;
                Ok(Self::from(s))
            }
        }
    };
}

#[cfg(test)]
mod tests {
    open_enum! {
        /// Test enum.
        pub enum Colour {
            /// Red.
            Red => "RED",
            /// Blue.
            Blue => "BLUE",
        }
    }

    #[test]
    fn round_trips_known_and_unknown() {
        let v: Vec<Colour> = serde_json::from_str(r#"["RED","GREEN"]"#).unwrap();
        assert_eq!(v, vec![Colour::Red, Colour::Unknown("GREEN".into())]);
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"["RED","GREEN"]"#);
        assert_eq!(Colour::Blue.to_string(), "BLUE");
        assert_eq!(Colour::from(String::from("BLUE")), Colour::Blue);
        assert_eq!(Colour::KNOWN, &["RED", "BLUE"]);
    }
}
