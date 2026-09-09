def policy(request: dict, context: dict) -> dict:
    # Context is supplied by the host, independently of worker Python objects.
    if request["scheme"] != "https" or request["port"] != 443:
        return {"action": "deny", "reason": "HTTPS is required"}

    if request["host"] == "status.example.com":
        return {"action": "respond", "status": 200, "body": "healthy"}

    if request["host"] == "api.example.com":
        headers = [
            pair for pair in request["headers"]
            if pair[0].lower() != "x-runtime-instance"
        ]
        return {
            "action": "forward",
            "headers": headers + [
                ("x-runtime-instance", context["execution"]["runtime_instance_id"])
            ],
        }

    return {"action": "deny", "reason": "destination is not allowed"}
