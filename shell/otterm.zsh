# Otterm ambient capture for zsh.        eval "$(otterm init zsh)"
#
# Wraps eligible interactive commands in `otterm run --` so their output is
# captured to the library automatically. Conservative by design: only plain,
# single, external commands are wrapped — pipes, redirects, control
# operators, assignments, builtins, functions, and aliases run untouched,
# because wrapping would change their semantics.
#
# Opt-outs:
#   - start the line with a space          (also keeps it out of history)
#   - otterm-off / otterm-on               (toggle for the session)
#   - OTTERM_IGNORE="foo bar"              (extra command names to skip)

[[ -o interactive ]] || return 0
(( ${+widgets} )) || return 0

# Commands that must own the terminal, or whose captures are escape-sequence
# noise rather than output worth keeping.
typeset -ga _otterm_ignore
_otterm_ignore=(
  otterm
  vim nvim vi nano emacs pico hx kak
  less more most man bat
  ssh mosh telnet
  tmux screen zellij
  top htop btop k9s
  fzf gdb lldb
  sudo su doas
  claude
)
if [[ -n $OTTERM_IGNORE ]]; then
  _otterm_ignore+=(${(s: :)OTTERM_IGNORE})
fi

# The per-capture footer is useful when you run otterm by hand; when every
# command is captured it's just noise.
export OTTERM_QUIET=1

otterm-off() { export OTTERM_DISABLE=1 }
otterm-on()  { unset OTTERM_DISABLE }

_otterm_accept_line() {
  emulate -L zsh
  local line=$BUFFER
  # Fast bail-outs: disabled, empty, opt-out leading space, multi-line, or
  # any shell syntax whose meaning wrapping would change.
  if [[ -n $OTTERM_DISABLE || -z $line || $line == \ * || $line == *$'\n'* ||
        $line == *[\|\;\&\<\>\`]* ]]; then
    zle .accept-line
    return
  fi
  local -a words
  words=(${(z)line})
  local first=${words[1]}
  # Skip env-assignment prefixes (FOO=bar cmd) and blocklisted names
  # (checked by basename, so /usr/bin/vim is caught too).
  if [[ $first == *=* ]] || (( ${_otterm_ignore[(Ie)${first:t}]} )); then
    zle .accept-line
    return
  fi
  # Only wrap external binaries. Builtins, functions, aliases, and reserved
  # words keep their exact semantics by running unwrapped.
  local kind
  kind=${"$(builtin whence -w -- $first 2>/dev/null)"##*: }
  if [[ $kind == command ]]; then
    # History keeps what you typed; the wrapped form stays out of it via
    # its leading space + HIST_IGNORE_SPACE.
    print -s -- $line
    BUFFER=" otterm run -- $line"
  fi
  zle .accept-line
}

setopt hist_ignore_space
zle -N accept-line _otterm_accept_line
