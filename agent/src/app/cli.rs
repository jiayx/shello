use crate::{Error, Result};

const SESSION_ID_ALPHABET: &str = "23456789abcdefghjkmnpqrstuvwxyz";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Config {
    pub(super) server: String,
    pub(super) session: Option<String>,
    pub(super) shell: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Command {
    Run(Config),
    Version,
}

pub(super) fn parse_args(args: impl Iterator<Item = String>) -> Result<Command> {
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
