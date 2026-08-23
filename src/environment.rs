pub(crate) const FORWARDED_VARS: &[&str] = &[
    "COLORTERM",
    "NO_COLOR",
    "TERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
];

pub(crate) fn terminal_env() -> Vec<(String, String)> {
    terminal_env_from(std::env::vars())
}

pub(crate) fn terminal_env_from<I>(source: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut forwarded: Vec<(String, String)> = source
        .into_iter()
        .filter(|(key, value)| FORWARDED_VARS.contains(&key.as_str()) && !value.is_empty())
        .collect();
    forwarded.sort_by(|(left, _), (right, _)| left.cmp(right));
    forwarded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<(String, String)> {
        vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("TERM".to_string(), "xterm-kitty".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
            ("TERM_PROGRAM".to_string(), "".to_string()),
            ("NO_COLOR".to_string(), "1".to_string()),
            ("TMUX".to_string(), "/tmp/tmux,1,0".to_string()),
        ]
    }

    #[test]
    fn forwards_only_known_terminal_vars_with_values() {
        assert_eq!(
            terminal_env_from(sample()),
            vec![
                ("COLORTERM".to_string(), "truecolor".to_string()),
                ("NO_COLOR".to_string(), "1".to_string()),
                ("TERM".to_string(), "xterm-kitty".to_string()),
            ]
        );
    }

    #[test]
    fn output_is_deterministic_regardless_of_source_order() {
        let mut reversed = sample();
        reversed.reverse();
        assert_eq!(terminal_env_from(sample()), terminal_env_from(reversed));
    }

    #[test]
    fn empty_environment_yields_nothing() {
        let empty: Vec<(String, String)> = Vec::new();
        assert!(terminal_env_from(empty).is_empty());
    }
}
