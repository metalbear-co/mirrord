FROM python:3.11-slim

# Install git (required for towncrier)
RUN apt-get update && apt-get install -y git && rm -rf /var/lib/apt/lists/*

# Fix "dubious ownership" error for mounted volumes
RUN git config --global --add safe.directory '*'

WORKDIR /app

# Install python dependencies
COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

# Copy script
COPY . .

# Move script to bin if needed, or just let users call it.
# The original setup had it in /usr/local/bin
RUN chmod +x calculate_version.py && ln -s /app/calculate_version.py /usr/local/bin/calculate_version.py

# We'll just default to bash so you can run `python /usr/local/bin/calculate_version.py` OR `towncrier`
ENTRYPOINT ["python3", "/usr/local/bin/calculate_version.py"]
