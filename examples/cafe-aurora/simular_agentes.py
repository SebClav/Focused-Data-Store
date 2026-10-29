"""Ejecuta, sin ningún modelo, las consultas que haría cada uno de los cuatro
agentes del ejemplo, hablando MCP con `fds mcp` (solo lectura).

Sirve para ver qué recibe cada agente y cuánto pesa, antes de gastar un solo
token de un modelo de verdad.

    python simular_agentes.py [ruta\\al\\fds.exe]
"""
import json, re, subprocess, sys
from pathlib import Path

AQUI = Path(__file__).resolve().parent
FDS = sys.argv[1] if len(sys.argv) > 1 else str(AQUI.parent.parent / "target" / "release" / "fds.exe")
DB = str(AQUI / "cafe-aurora.fds")
CONTEXTO = "Sitio web de Café Aurora"

# Qué pregunta cada agente de su propio trabajo. La marca (tono, colores,
# tipografías) ya viene en el contexto: no se vuelve a consultar.
AGENTES = {
    "estructura": ["armar la página de inicio de un negocio local"],
    "textos": ["horario de atención", "carta de cafés y precios", "carta de pastelería",
               "estadísticas del negocio"],
    "imagenes": ["foto de la fachada del local", "foto del espresso", "foto de la pastelería",
                 "foto del equipo"],
    "revision": ["comprobar accesibilidad de una página simple"],
}


class Mcp:
    def __init__(self):
        self.p = subprocess.Popen([FDS, "--db", DB, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, encoding="utf-8")
        self.n = 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18"})

    def rpc(self, method, params=None):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}) + "\n")
        self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())

    def tool(self, herramienta, **args):
        r = self.rpc("tools/call", {"name": herramienta, "arguments": args})
        return r["result"]["content"][0]["text"]

    def close(self):
        self.p.kill()


def palabras(t):
    return len(t.split())


def main():
    base = subprocess.run([FDS, "--db", DB, "--json", "ls"], capture_output=True, text=True, encoding="utf-8")
    todo = sum(r["tokens"] for r in json.loads(base.stdout))
    print(f"Toda la base, pegada en un prompt: {todo} palabras\n")

    total = 0
    for nombre, preguntas in AGENTES.items():
        m = Mcp()
        print(f"── agente {nombre} " + "─" * (50 - len(nombre)))
        ctx = m.tool("fds_context", name=CONTEXTO)
        h = re.search(r"hash: ([0-9a-f]+)", ctx).group(1)
        print(f"  carga el contexto una vez: {palabras(ctx)} palabras (hash {h})")
        again = m.tool("fds_context", name=CONTEXTO, known_hash=h)
        print(f"  la segunda vez que lo pide: {palabras(again)} palabras -> \"{again[:60]}…\"")
        gasto = palabras(ctx)
        for q in preguntas:
            r = m.tool("fds_query", query=q)
            conf = re.search(r"confidence: (\w+)", r).group(1)
            gasto += palabras(r)
            primera = r.split("\n")[3] if len(r.split("\n")) > 3 else ""
            print(f"  consulta «{q}» -> {conf}, {palabras(r)} palabras")
        print(f"  total del agente: {gasto} palabras\n")
        total += gasto
        m.close()
    print(f"Los cuatro agentes juntos: {total} palabras.")
    print(f"Si cada uno recibiera la base entera: {todo * len(AGENTES)} palabras.")


if __name__ == "__main__":
    main()
