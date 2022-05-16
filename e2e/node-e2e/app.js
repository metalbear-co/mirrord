const express = require('express');
const process = require('process');
const fs = require('fs');
const app = express();
const PORT = 80;

const TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";


app.get('/', (req, res) => {
    res.send('Request received'); // Todo: validate responses 
    console.log("GET: Request completed");
});

app.post('/', (req, res) => {
    req.on('data', (data) => {
        if (data.toString() == TEXT) {
            console.log("POST: Request completed");
        }
    });
});

app.put('/', (req, res) => {
    req.on('data', (data) => {
        fs.writeFile('/tmp/test', data.toString(), (err) => {
            if (err) {
                throw err;
            }
        });
    });
    console.log("PUT: Request completed");
});

app.delete('/', (req, res) => {
    req.on('data', (data) => {
        fs.unlink('/tmp/test', (err) => {
            if (err) {
                throw err;
            }
        });
    });
    console.log("DELETE: Request completed");
});

app.listen(PORT, () => {
    console.log(`Server listening on port ${PORT}`);
});