use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        process::CommandExt,
    },
    path::Path,
    process::Command,
};

const SDL_IGNORE: &str = "SDL_GAMECONTROLLER_IGNORE_DEVICES";
const SDL_ALLOW: &str = "SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT";
const DUALSENSE_IDS: &str = "0x054c/0x0ce6,0x054c/0x0df2";

pub fn run(mut args: impl Iterator<Item = OsString>) -> io::Error {
    let separator = args.next();
    let program = args.next().filter(|program| !program.is_empty());
    let (Some(separator), Some(program)) = (separator, program) else {
        return missing_command();
    };
    if separator != "--" {
        return missing_command();
    }
    if env::var_os(SDL_ALLOW).is_some_and(|list| !list.is_empty()) {
        return io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{SDL_ALLOW} is set and takes precedence over {SDL_IGNORE}"),
        );
    }
    let ignore_list = match ignore_list(env::var_os(SDL_IGNORE).unwrap_or_default()) {
        Ok(list) => list,
        Err(err) => return err,
    };

    let err = Command::new(&program)
        .args(args)
        .env(SDL_IGNORE, ignore_list)
        .exec();
    io::Error::new(
        err.kind(),
        format!("could not launch {}: {err}", program.to_string_lossy()),
    )
}

fn missing_command() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "usage: joyshockrust-linux --launch-sdl -- COMMAND [ARGS...]",
    )
}

fn ignore_list(mut existing: OsString) -> io::Result<OsString> {
    if let Some(path) = existing.as_bytes().strip_prefix(b"@") {
        let path = Path::new(OsStr::from_bytes(path));
        let mut contents = fs::read(path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("could not load {SDL_IGNORE} file {}: {err}", path.display()),
            )
        })?;
        if let Some(nul) = contents.iter().position(|&byte| byte == 0) {
            contents.truncate(nul);
        }
        existing = OsString::from_vec(contents);
    }
    if !existing.is_empty() {
        existing.push(",");
    }
    existing.push(DUALSENSE_IDS);
    Ok(existing)
}
