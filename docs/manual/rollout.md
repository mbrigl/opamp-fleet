       http://127.0.0.1:4321/api/v1/agents/<instance_uid>/rollout
       http://127.0.0.1:4321/api/v1/configurations/promtail-conf
$ curl -X POST http://127.0.0.1:4321/api/v1/configurations/promtail-conf/rollout
$ curl -s http://127.0.0.1:4321/api/v1/agents | jq '.[] | select(.service_name=="promtail")'
