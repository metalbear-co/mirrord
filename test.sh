FILE=/app/test.txt
if [ -f "$FILE" ]; then
    echo "$FILE exists."
    exit 0
else 
    echo "$FILE does not exist."
    exit 1
fi