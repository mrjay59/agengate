import requests, json, time

url = "http://localhost:8787/v1/chat/completions"
headers = {"Content-Type": "application/json"}

# Test models endpoint first
models_url = "http://localhost:8787/v1/models"
models_resp = requests.get(models_url)
print("models status", models_resp.status_code)
models_data = models_resp.json()
print("models count:", len(models_data['data']))

# Use first model from the list
first_model = models_data['data'][0]['id']
print("Testing with model:", first_model)

body = {
    "model": "opencode/beam-free",
    "messages": [
        {"role": "user", "content": "Hello"}
    ]
}
start = time.time()
resp = requests.post(url, headers=headers, data=json.dumps(body))
print("status", resp.status_code)
print("response", resp.json())
print("latency", time.time()-start)
