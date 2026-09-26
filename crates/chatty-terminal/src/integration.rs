//! Shell integration: small startup files that make bash and zsh print
//! OSC 133 marks, loaded without touching the user's dotfiles.
//!
//! - bash runs with `--rcfile <ours>`. Ours sources what bash would have:
//!   `~/.bashrc`, or for a login shell `/etc/profile` and the first of
//!   `~/.bash_profile`/`~/.bash_login`/`~/.profile` (bash itself still reads
//!   `/etc/bash.bashrc` first, as it does for any interactive shell). Then it
//!   wraps the prompt. The shell is no longer a login shell to bash itself
//!   (`shopt login_shell` is off).
//! - zsh runs with `ZDOTDIR` pointing at our directory. Our `.zshenv`,
//!   `.zprofile` and `.zshrc` each source the user's own file from their
//!   real `ZDOTDIR` (or `$HOME`) first, and `.zshrc` restores `ZDOTDIR`.
//!
//! The marks go around whatever prompt the user's files set, re-applied at
//! each prompt so prompt frameworks that rebuild `PS1` keep them. A shell
//! that already prints `133;A` in its prompt is left alone. fish and
//! PowerShell are not supported; they run without integration.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const BASH_RC: &str = r#"# chatty shell integration for bash (OSC 133). Sources your own startup
# files first; nothing here is written to them.
if [ -n "${__CHATTY_LOGIN-}" ]; then
    unset __CHATTY_LOGIN
    [ -r /etc/profile ] && . /etc/profile
    for __chatty_rc in ~/.bash_profile ~/.bash_login ~/.profile; do
        if [ -r "$__chatty_rc" ]; then
            . "$__chatty_rc"
            break
        fi
    done
    unset __chatty_rc
elif [ -r ~/.bashrc ]; then
    . ~/.bashrc
fi

if [ -z "${__chatty_osc133-}" ] && [[ "$PS1" != *'133;A'* ]]; then
    __chatty_osc133=1
    __chatty_prompt_start() {
        local ec=$?
        printf '\e]133;D;%s\a' "$ec"
        return "$ec"
    }
    __chatty_prompt_end() {
        local ec=$?
        if [ "$PS1" != "${__chatty_ps1-}" ]; then
            __chatty_ps1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
            PS1=$__chatty_ps1
        fi
        return "$ec"
    }
    PS0='\e]133;C\a'"${PS0-}"
    if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
        PROMPT_COMMAND=(__chatty_prompt_start "${PROMPT_COMMAND[@]}" __chatty_prompt_end)
    else
        __chatty_nl=$'\n'
        PROMPT_COMMAND="__chatty_prompt_start${__chatty_nl}${PROMPT_COMMAND:+$PROMPT_COMMAND$__chatty_nl}__chatty_prompt_end"
        unset __chatty_nl
    fi
    # The hooks exist only in this shell; a child shell must not inherit them.
    export -n PROMPT_COMMAND
fi
"#;

const ZSH_ENV: &str = r#"# chatty shell integration for zsh (OSC 133). ZDOTDIR points here; each
# file sources your own from your ZDOTDIR first.
__chatty_zdotdir=$ZDOTDIR
ZDOTDIR=${__CHATTY_USER_ZDOTDIR:-$HOME}
[[ -r $ZDOTDIR/.zshenv ]] && source $ZDOTDIR/.zshenv
__CHATTY_USER_ZDOTDIR=$ZDOTDIR
ZDOTDIR=$__chatty_zdotdir
"#;

const ZSH_PROFILE: &str = r#"ZDOTDIR=$__CHATTY_USER_ZDOTDIR
[[ -r $ZDOTDIR/.zprofile ]] && source $ZDOTDIR/.zprofile
ZDOTDIR=$__chatty_zdotdir
"#;

const ZSH_RC: &str = r#"ZDOTDIR=$__CHATTY_USER_ZDOTDIR
[[ -r $ZDOTDIR/.zshrc ]] && source $ZDOTDIR/.zshrc
# Leave ZDOTDIR as it would have been (zsh reads .zlogin from it next).
if [[ -z ${__CHATTY_ZDOTDIR_SET-} ]]; then
    if [[ $ZDOTDIR == $HOME ]]; then unset ZDOTDIR; else typeset +x ZDOTDIR; fi
fi
unset __CHATTY_USER_ZDOTDIR __CHATTY_ZDOTDIR_SET __chatty_zdotdir

if [[ -z ${__chatty_osc133-} && $PS1 != *'133;A'* ]]; then
    __chatty_osc133=1
    __chatty_precmd() {
        local ec=$?
        printf '\e]133;D;%s\a' $ec
        return $ec
    }
    __chatty_wrap_prompt() {
        if [[ $PS1 != "${__chatty_ps1-}" ]]; then
            __chatty_ps1=$'%{\e]133;A\a%}'$PS1$'%{\e]133;B\a%}'
            PS1=$__chatty_ps1
        fi
    }
    __chatty_preexec() {
        printf '\e]133;C\a'
    }
    # `-` defaults: the user's .zshrc may have left `nounset` on.
    precmd_functions=(__chatty_precmd ${precmd_functions-} __chatty_wrap_prompt)
    preexec_functions=(${preexec_functions-} __chatty_preexec)
fi
"#;

/// Which shell `program` is, if it is one with integration.
fn shell_kind(program: &str) -> Option<&'static str> {
    match Path::new(program).file_name()?.to_str()? {
        "bash" => Some("bash"),
        "zsh" => Some("zsh"),
        _ => None,
    }
}

/// Set up integration for `program` started as a `login` shell or not:
/// returns its arguments and adds to `env`, or `None` when the shell has no
/// integration (or its files could not be written; that is logged).
pub(crate) fn inject(
    program: &str,
    login: bool,
    env: &mut HashMap<String, String>,
) -> Option<Vec<String>> {
    let kind = shell_kind(program)?;
    let dir = match files_dir() {
        Ok(dir) => dir,
        Err(e) => {
            tracing::warn!("terminal: shell integration off: {e}");
            return None;
        }
    };
    let mut args = Vec::new();
    match kind {
        "bash" => {
            if login {
                env.insert("__CHATTY_LOGIN".into(), "1".into());
            }
            args.push("--rcfile".into());
            args.push(dir.join("bashrc").to_string_lossy().into_owned());
        }
        _ => {
            let user_zdotdir = env
                .get("ZDOTDIR")
                .cloned()
                .or_else(|| std::env::var("ZDOTDIR").ok());
            if let Some(user) = user_zdotdir {
                env.insert("__CHATTY_USER_ZDOTDIR".into(), user);
                env.insert("__CHATTY_ZDOTDIR_SET".into(), "1".into());
            }
            env.insert(
                "ZDOTDIR".into(),
                dir.join("zsh").to_string_lossy().into_owned(),
            );
            if login {
                args.push("-l".into());
            }
        }
    }
    args.push("-i".into());
    Some(args)
}

/// The integration files, written once per process into a new private
/// directory (under `$XDG_RUNTIME_DIR` when set, else the temp dir). A fresh
/// directory, created 0700, so nobody else can have planted files in it.
fn files_dir() -> Result<&'static Path, String> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| write_files().map_err(|e| format!("writing its startup files: {e}")))
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(Clone::clone)
}

fn write_files() -> io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = base.join(format!(
        "chatty-shell-integration-{}-{nanos}",
        std::process::id()
    ));
    // `create` fails if the path exists, so the directory is ours.
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(dir.join("zsh"))?;
    let files = [
        ("bashrc", BASH_RC),
        ("zsh/.zshenv", ZSH_ENV),
        ("zsh/.zprofile", ZSH_PROFILE),
        ("zsh/.zshrc", ZSH_RC),
    ];
    for (name, content) in files {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join(name))?;
        io::Write::write_all(&mut file, content.as_bytes())?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bash_and_zsh_get_integration() {
        let mut env = HashMap::new();
        assert!(inject("/bin/sh", true, &mut env).is_none());
        assert!(inject("fish", true, &mut env).is_none());
        assert!(env.is_empty());

        let args = inject("/usr/bin/bash", true, &mut env).unwrap();
        assert_eq!(args[0], "--rcfile");
        assert_eq!(args[2], "-i");
        assert_eq!(env["__CHATTY_LOGIN"], "1");
        let rc = std::fs::read_to_string(&args[1]).unwrap();
        assert_eq!(rc, BASH_RC);

        let mut env = HashMap::new();
        assert_eq!(inject("zsh", false, &mut env).unwrap(), vec!["-i"]);
        assert!(Path::new(&env["ZDOTDIR"]).join(".zshrc").is_file());
    }

    #[test]
    fn zsh_hooks_expand_with_defaults_under_nounset() {
        // The hook arrays may be unset; under `nounset` a bare `$name`
        // aborts the rc file before the hooks install.
        for name in ["precmd_functions", "preexec_functions"] {
            assert!(ZSH_RC.contains(&format!("${{{name}-}}")), "{name}");
            assert!(!ZSH_RC.contains(&format!("${name}")), "{name}");
        }
    }
}
