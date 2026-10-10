#!/bin/bash
# vLLM token counters every 15 s (epoch, prompt_tokens_total, generation_tokens_total,
# successful requests): worker usage does not reach the leader's usage file inside the
# Harbor containers (see RESUME.md), so arm totals and arm-B per-trial tokens come from here.
while true; do
  m=$(curl -s --max-time 5 http://172.17.0.1:8000/metrics)
  p=$(grep '^vllm:prompt_tokens_total' <<<"$m" | awk '{print $2}')
  g=$(grep '^vllm:generation_tokens_total' <<<"$m" | awk '{print $2}')
  r=$(grep '^vllm:request_success_total' <<<"$m" | awk '{s+=$2} END {print s}')
  echo "$(date +%s) $p $g $r" >> /media/marcel/data/rust/swarm-results/age-862/meter.log
  sleep 15
done
