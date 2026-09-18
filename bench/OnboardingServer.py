#!/usr/bin/env python3
"""Loopback-only synthetic Ollama responses; never downloads or runs a model."""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.reply({'models': [{'name': 'fixture:local'}, {'name': 'alias', 'remote_host': 'https://example.invalid'}]})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
        model = body.get('model', '')
        if self.path == '/api/show':
            if model == 'redirect':
                self.send_response(302)
                self.send_header('Location', 'https://example.invalid/private')
                self.end_headers()
                return
            if model == 'huge':
                self.reply({'padding': 'x' * (2 * 1024 * 1024 + 1)})
                return
            data = {'details': {'format': 'gguf'}, 'model_info': {'general.architecture': 'fixture'},
                    'capabilities': ['completion', 'embedding']}
            if model == 'alias':
                data['remote_model'] = 'remote'
                data['remote_host'] = 'https://example.invalid'
            self.reply(data)
        elif self.path == '/api/generate':
            if model == 'alias':
                raise RuntimeError('Cloud alias reached generation')
            self.reply({'done': True, 'response': 'Hello.'})
        elif self.path == '/api/embed':
            self.reply({'embeddings': [[0.1, 0.2, 0.3]]})
        elif self.path == '/api/pull':
            self.send_response(200)
            self.end_headers()
            lines = [{'status': 'pulling layer', 'completed': 1, 'total': 10}, {'status': 'success'}]
            if model == 'qwen3.5:0.8b':
                lines = [{'status': 'pulling layer', 'completed': 1, 'total': 10}]
            for line in lines:
                try:
                    self.wfile.write(json.dumps(line).encode() + b'\n')
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    return
                if model == 'qwen3.5:2b':
                    time.sleep(2)
        else:
            self.reply({'error': 'unsupported'})

    def reply(self, body):
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass


server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
Path(sys.argv[1]).write_text(str(server.server_address[1]))
server.serve_forever()
