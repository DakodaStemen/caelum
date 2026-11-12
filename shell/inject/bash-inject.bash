# Caelum automatic bash integration. Caelum starts bash as `bash --posix` with ENV
# pointing here (bash only reads ENV in POSIX mode). This undoes POSIX mode, then
# reproduces what the plain `bash -l` Caelum starts without injection would do:
# /etc/profile and the first of ~/.bash_profile, ~/.bash_login, ~/.profile. As with
# `bash -l`, ~/.bashrc is read only if one of those files sources it. Finally it
# loads Caelum's own integration.
if [ -n "${CAELUM_BASH_INJECT-}" ]; then
  builtin unset CAELUM_BASH_INJECT
  # Give child shells the ENV the user had, not ours.
  if [ -n "${CAELUM_BASH_ENV-}" ]; then
    ENV=$CAELUM_BASH_ENV
  else
    builtin unset ENV
  fi
  builtin unset CAELUM_BASH_ENV
  builtin set +o posix
  # `--posix` also switched these on/off; put back what a plain interactive bash has.
  # (inherit_errexit needs bash >= 4.4, hence the redirects.)
  builtin shopt -u inherit_errexit 2>/dev/null
  builtin shopt -u shift_verbose 2>/dev/null
  : "${HISTSIZE=500}" "${HISTFILESIZE=500}"
  __caelum_inject_dir=${BASH_SOURCE[0]%/*}
  if [ -z "${HISTFILE-}" ] || [ "$HISTFILE" = "$HOME/.sh_history" ]; then
    HISTFILE=$HOME/.bash_history
  fi
  [ -r /etc/profile ] && builtin source /etc/profile
  for __caelum_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [ -r "$__caelum_f" ]; then
      builtin source "$__caelum_f"
      break
    fi
  done
  [ -r "$__caelum_inject_dir/../caelum.bash" ] && builtin source "$__caelum_inject_dir/../caelum.bash"
  builtin unset __caelum_f __caelum_inject_dir
fi
