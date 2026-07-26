use crate::{Error, Result};

const SESSION_ID_ALPHABET: &str = "23456789abcdefghjkmnpqrstuvwxyz";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Config {
    pub(crate) server: String,
    pub(crate) session: Option<String>,
    pub(crate) shell: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Run(Config),
    Version,
}

pub(crate) fn parse_args(args: impl Iterator<Item = String>) -> Result<Command> {
    let mut config = Config {
        server: "http://localhost:5173".to_string(),
        session: None,
        shell: None,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        if matches!(arg.as_str(), "--version" | "-V") {
            return Ok(Command::Version);
        }
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(left, right)| (left, Some(right)));
        let mut value = || -> Result<String> {
            inline
                .map(str::to_string)
                .or_else(|| args.next())
                .ok_or_else(|| Error::Message(format!("missing value for {name}")))
        };
        match name {
            "--server" | "-server" => config.server = value()?,
            "--session" | "-session" => config.session = Some(value()?),
            "--shell" | "-shell" => config.shell = Some(value()?),
            _ => return Err(Error::Message(format!("unknown argument: {arg}"))),
        }
    }
    if config.server.is_empty() {
        return Err(Error::Message("server URL must not be empty".into()));
    }
    if let Some(session) = config.session.as_deref() {
        validate_session_id(session)?;
    }
    Ok(Command::Run(config))
}

fn validate_session_id(session_id: &str) -> Result<()> {
    let valid = session_id.len() == 7
        && session_id.as_bytes().get(3) == Some(&b'-')
        && session_id
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 3 || SESSION_ID_ALPHABET.as_bytes().contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(Error::Message(format!("invalid session ID: {session_id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arguments_and_validates_session_ids() {
        assert_eq!(
            parse_args(
                [
                    "--server=https://example.test:8443/base".to_string(),
                    "--session".to_string(),
                    "abc-def".to_string(),
                    "--shell=/bin/bash".to_string(),
                ]
                .into_iter(),
            )
            .unwrap(),
            Command::Run(Config {
                server: "https://example.test:8443/base".to_string(),
                session: Some("abc-def".to_string()),
                shell: Some("/bin/bash".to_string()),
            })
        );

        assert!(parse_args(["--session".to_string(), "abc-io0".to_string()].into_iter()).is_err());
        assert!(parse_args(["--server=".to_string()].into_iter()).is_err());
        assert!(parse_args(["--unknown".to_string()].into_iter()).is_err());
    }

    #[test]
    fn parses_version_arguments() {
        assert_eq!(
            parse_args(["--version".to_string()].into_iter()).unwrap(),
            Command::Version
        );
        assert_eq!(
            parse_args(["-V".to_string()].into_iter()).unwrap(),
            Command::Version
        );
    }
}
