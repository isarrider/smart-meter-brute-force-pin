FROM python:3.11-slim

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY smart-meter-brute-force-pin.py .

# Checkpoint lives here so progress survives a container restart.
# Mount a named volume at /data to persist it across `docker run`s.
VOLUME ["/data"]

CMD ["python", "smart-meter-brute-force-pin.py"]
