#!/usr/bin/env bash
# Claude Code statusline — ticket/branch | model·effort | context % | cost.
# Invoked by settings.json statusLine; reads the session JSON on stdin.
# Requires: jq. Label priority: ticket from branch → branch → folder.

input=$(cat)

eval "$(jq -r '
  @sh "PCT=\(.context_window.used_percentage // 0 | floor)",
  @sh "COST=\(.cost.total_cost_usd // 0)",
  @sh "DIR=\(.workspace.current_dir // .cwd // "")",
  @sh "MODEL_ID=\(.model.id // "")",
  @sh "MODEL_NAME=\(.model.display_name // "")",
  @sh "EFFORT=\(.effort.level // "")"
' <<<"$input" 2>/dev/null)" || { printf '??'; exit 0; }

# --- Label: ticket from branch, else branch, else folder ---
TICKET="" DESC=""
BRANCH=$(git -C "$DIR" symbolic-ref --short HEAD 2>/dev/null) || true
if [[ "$BRANCH" =~ ([A-Z][A-Z0-9]*-[0-9]+) ]]; then
  TICKET="${BASH_REMATCH[1]}"
  DESC="${BRANCH##*"$TICKET"}"
  DESC="${DESC#-}"
  [ ${#DESC} -gt 24 ] && DESC="${DESC:0:21}..."
elif [ -n "$BRANCH" ]; then
  DESC="$BRANCH"
  [ ${#DESC} -gt 30 ] && DESC="${DESC:0:27}..."
else
  DESC=$(basename "$DIR" 2>/dev/null)
fi

# --- Colors ---
C_TICKET='\e[38;5;75m'   # bright blue
C_DESC='\e[38;5;252m'    # light gray
C_SEP='\e[38;5;240m'     # dim gray
C_COST='\e[38;5;114m'    # soft green
C_EFF='\e[38;5;245m'     # mid gray
RST='\e[0m'

# Context % heat
if   [ "$PCT" -le 30 ]; then C_PCT='\e[32m'         # green
elif [ "$PCT" -le 60 ]; then C_PCT='\e[33m'         # yellow
elif [ "$PCT" -le 80 ]; then C_PCT='\e[38;5;208m'   # orange
else                         C_PCT='\e[31m'; fi     # red

# --- Model + effort chip ---
MODEL_SHORT="" MODEL_COLOR='\e[38;5;245m'
case "$MODEL_ID" in
  *opus*)   MODEL_SHORT="opus";   MODEL_COLOR='\e[38;2;74;163;255m'  ;;
  *sonnet*) MODEL_SHORT="sonnet"; MODEL_COLOR='\e[38;2;45;212;191m'  ;;
  *fable*)  MODEL_SHORT="fable";  MODEL_COLOR='\e[38;2;157;92;255m'  ;;
  *haiku*)  MODEL_SHORT="haiku";  MODEL_COLOR='\e[38;2;74;222;128m'  ;;
  *)        MODEL_SHORT=$(printf '%s' "$MODEL_NAME" | awk '{print tolower($1)}') ;;
esac
CHIP="${MODEL_COLOR}\e[1m${MODEL_SHORT}${RST}"
[ -n "$EFFORT" ] && CHIP+="${C_SEP}·${RST}${C_EFF}${EFFORT}${RST}"

# --- Assemble ---
if [ -n "$TICKET" ] && [ -n "$DESC" ]; then
  OUT="${C_TICKET}\e[1m${TICKET}${RST} ${C_DESC}${DESC}${RST}"
elif [ -n "$TICKET" ]; then
  OUT="${C_TICKET}\e[1m${TICKET}${RST}"
else
  OUT="${C_DESC}\e[1m${DESC:-claude}${RST}"
fi
OUT+="  ${C_SEP}|${RST}  ${CHIP}"
OUT+="  ${C_SEP}|${RST}  ${C_PCT}${PCT}%${RST}"

COST_FMT=$(printf '$%.2f' "$COST" 2>/dev/null || echo '$0.00')
[ "$COST_FMT" != '$0.00' ] && OUT+="  ${C_SEP}|${RST}  ${C_COST}${COST_FMT}${RST}"

printf '%b' "$OUT"
