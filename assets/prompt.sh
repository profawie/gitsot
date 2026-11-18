# Gitsot prompt for zsh and bash.
#
# Sourced only inside Gitsot's built-in terminal, after your own startup
# files. Before every prompt it prints one line of labelled chips (folder,
# branch, sync, changes, identity, last command) and then a short prompt.
# The icons are private-use characters that Gitsot's terminal draws itself,
# so no special font is needed.
#
# Turn it off in Gitsot: Settings > Terminal > Gitsot prompt.

__gitsot_cap_l=$'\xee\x82\xb6'
__gitsot_cap_r=$'\xee\x82\xb4'
__gitsot_i_folder=$'\xee\xb8\x80'
__gitsot_i_branch=$'\xee\xb8\x81'
__gitsot_i_person=$'\xee\xb8\x82'
__gitsot_i_pencil=$'\xee\xb8\x83'
__gitsot_i_clock=$'\xee\xb8\x84'
__gitsot_i_check=$'\xee\xb8\x85'
__gitsot_i_cross=$'\xee\xb8\x86'
__gitsot_i_up=$'\xee\xb8\x87'
__gitsot_i_down=$'\xee\xb8\x88'
__gitsot_i_warn=$'\xee\xb8\x89'
__gitsot_i_prompt=$'\xee\xb8\x8a'

__gitsot_first=1
__gitsot_start=

# __gitsot_chip "R;G;B" "text": a rounded chip with white text.
__gitsot_chip() {
  printf '\033[38;2;%sm%s\033[48;2;%sm\033[1;38;2;255;255;255m%s\033[0m\033[38;2;%sm%s\033[0m ' \
    "$1" "$__gitsot_cap_l" "$1" "$2" "$1" "$__gitsot_cap_r"
}

# __gitsot_line <exit code of the last command> <seconds it took, or empty>
__gitsot_line() {
  local code=$1 took=$2 dir=$PWD line rest xy x y text
  local branch= oid= upstream= ahead=0 behind=0 staged=0 changed=0 new=0 conflicts=0 email=

  case "$dir/" in
    "${GITSOT_REPO:-//}"/*) dir="${GITSOT_REPO##*/}${dir#"$GITSOT_REPO"}" ;;
    "$HOME"/*) dir="~${dir#"$HOME"}" ;;
  esac

  # Window title; Gitsot also takes it as "a command finished" and refreshes.
  printf '\033]0;Gitsot: %s\007' "$dir"
  if [ -n "$__gitsot_first" ]; then __gitsot_first=; else printf '\n'; fi

  __gitsot_chip "11;107;168" "$__gitsot_i_folder  $dir"

  if rest=$(command git --no-optional-locks status --porcelain=v2 --branch 2>/dev/null); then
    while IFS= read -r line; do
      case "$line" in
        "# branch.head "*) branch=${line#"# branch.head "} ;;
        "# branch.oid "*) oid=${line#"# branch.oid "} ;;
        "# branch.upstream "*) upstream=1 ;;
        "# branch.ab "*)
          line=${line#"# branch.ab +"}
          ahead=${line%% *}
          behind=${line##*-}
          ;;
        "u "*) conflicts=$((conflicts + 1)) ;;
        "? "*) new=$((new + 1)) ;;
        "1 "* | "2 "*)
          xy=${line#??}
          x=${xy%"${xy#?}"}
          y=${xy#?}
          y=${y%"${y#?}"}
          if [ "$x" != . ]; then staged=$((staged + 1)); fi
          if [ "$y" != . ]; then changed=$((changed + 1)); fi
          ;;
      esac
    done <<__GITSOT_EOF__
$rest
__GITSOT_EOF__

    if [ "$branch" = "(detached)" ]; then
      branch="detached at $(printf '%.7s' "$oid")"
    fi
    __gitsot_chip "15;118;110" "$__gitsot_i_branch  $branch"

    if [ -z "$upstream" ]; then
      case "$branch" in
        detached*) ;;
        *) __gitsot_chip "71;85;105" "not published" ;;
      esac
    elif [ "$ahead" != 0 ] || [ "$behind" != 0 ]; then
      text=
      if [ "$ahead" != 0 ]; then text="$__gitsot_i_up  $ahead to push"; fi
      if [ "$behind" != 0 ]; then text="${text:+$text  }$__gitsot_i_down  $behind to pull"; fi
      __gitsot_chip "71;85;105" "$text"
    fi

    if [ "$conflicts" != 0 ]; then
      __gitsot_chip "185;28;28" "$__gitsot_i_warn  $conflicts in conflict"
    fi
    if [ "$staged" != 0 ] || [ "$changed" != 0 ] || [ "$new" != 0 ]; then
      text=
      if [ "$staged" != 0 ]; then text="$staged staged"; fi
      if [ "$changed" != 0 ]; then text="${text:+$text, }$changed changed"; fi
      if [ "$new" != 0 ]; then text="${text:+$text, }$new new"; fi
      __gitsot_chip "180;83;9" "$__gitsot_i_pencil  $text"
    elif [ "$conflicts" = 0 ]; then
      __gitsot_chip "21;128;61" "$__gitsot_i_check  clean"
    fi

    email=$(command git config user.email 2>/dev/null)
    if [ -n "$email" ]; then
      __gitsot_chip "109;40;217" "$__gitsot_i_person  $email"
    else
      __gitsot_chip "185;28;28" "$__gitsot_i_person  no identity"
    fi
  fi

  if [ -n "$took" ] && [ "$took" -ge 3 ]; then
    if [ "$took" -ge 60 ]; then
      text="$((took / 60))m $((took % 60))s"
    else
      text="${took}s"
    fi
    __gitsot_chip "71;85;105" "$__gitsot_i_clock  $text"
  fi
  if [ "$code" != 0 ]; then
    __gitsot_chip "185;28;28" "$__gitsot_i_cross  exit $code"
  fi
  printf '\n'
}

if [ -n "$ZSH_VERSION" ]; then
  zmodload zsh/datetime 2>/dev/null
  __gitsot_preexec() { __gitsot_start=$EPOCHSECONDS; }
  __gitsot_precmd() {
    local code=$? took=
    if [ -n "$__gitsot_start" ] && [ -n "$EPOCHSECONDS" ]; then
      took=$((EPOCHSECONDS - __gitsot_start))
    fi
    __gitsot_start=
    __gitsot_line "$code" "$took"
    # Set here, after every other hook, so a theme cannot replace it.
    PROMPT="%{%(?.%F{green}.%F{red})%}${__gitsot_i_prompt}%{%f%} "
    RPROMPT=
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook preexec __gitsot_preexec
  add-zsh-hook precmd __gitsot_precmd
elif [ -n "$BASH_VERSION" ]; then
  __gitsot_precmd() {
    local code=$?
    __gitsot_line "$code" ""
    if [ "$code" = 0 ]; then
      PS1='\[\033[32m\]'"$__gitsot_i_prompt"'\[\033[0m\] '
    else
      PS1='\[\033[31m\]'"$__gitsot_i_prompt"'\[\033[0m\] '
    fi
  }
  PROMPT_COMMAND=__gitsot_precmd
fi
