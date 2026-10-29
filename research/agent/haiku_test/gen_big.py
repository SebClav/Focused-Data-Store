"""Builds negocio_grande.json: the café's 18 records hidden among ~500 others,
some of them near-collisions (another business's hours, prices, colours, photos)."""
import json, random
from pathlib import Path

HERE = Path(__file__).parent
base = json.load(open(HERE.parent.parent.parent / "examples" / "cafe-aurora" / "negocio.json", encoding="utf-8"))
rng = random.Random(11)

TOPICS = ["facturación", "inventario", "proveedores", "uniformes", "limpieza", "capacitación", "seguridad", "reclamos",
          "garantías", "devoluciones", "alquiler", "permisos municipales", "mantenimiento", "transporte", "marketing",
          "redes sociales", "contabilidad", "nómina", "vacaciones", "turnos", "compras", "almacén", "cadena de frío",
          "residuos", "auditoría", "software de caja", "programa de fidelidad", "eventos privados", "delivery", "capacidad del local"]
VERBS = ["se registra", "se revisa", "se aprueba", "se archiva", "se comunica", "se actualiza", "se entrega", "se cobra", "se programa", "se cancela"]
WHO = ["el encargado", "administración", "el turno de la mañana", "el turno de la tarde", "contabilidad", "el dueño", "el equipo de cocina", "la persona de caja"]
WHEN = ["cada lunes", "al cierre del día", "dentro de 48 horas", "el primer día del mes", "antes de las 10:00", "cada quincena", "al recibir el pedido", "con una semana de anticipación"]
EXTRA = ["Debe quedar constancia por escrito.", "Si hay diferencias, se avisa al dueño.", "El formato está en la carpeta compartida.", "Se guarda una copia durante un año.",
         "No se hacen excepciones salvo autorización.", "Las dudas van a administración.", "Se informa por el grupo del equipo."]

filler = []
for i in range(480):
    t = rng.choice(TOPICS)
    n = rng.randint(1, 3)
    body = " ".join(f"En {t}, {rng.choice(WHO)} {rng.choice(VERBS)} {rng.choice(WHEN)}. {rng.choice(EXTRA)}" for _ in range(n + 1))
    filler.append({"kind": "fact", "collection": "operaciones", "question": f"procedimiento de {t} número {i + 1}", "answer": body, "source": "manual interno"})

traps = [
    {"kind": "fact", "collection": "operaciones", "question": "horario de atención del taller de mantenimiento", "answer": "Lunes a viernes de 8:30 a 17:30.", "source": "manual interno"},
    {"kind": "fact", "collection": "operaciones", "question": "dirección del almacén central", "answer": "Jr. Los Pinos 340, Ate, Lima.", "source": "manual interno"},
    {"kind": "fact", "collection": "operaciones", "question": "teléfono y whatsapp del proveedor de granos", "answer": "WhatsApp +51 988 000 111. Correo ventas@granoselsol.pe.", "source": "proveedor"},
    {"kind": "fact", "collection": "operaciones", "question": "carta de vinos y precios del restaurante vecino", "answer": "Malbec S/ 45, Merlot S/ 52, Rosé S/ 38.", "source": "competencia"},
    {"kind": "fact", "collection": "operaciones", "question": "carta de postres del restaurante vecino", "answer": "Suspiro limeño S/ 18, Tres leches S/ 16, Helado artesanal S/ 12.", "source": "competencia"},
    {"kind": "fact", "collection": "operaciones", "question": "estadísticas de la cadena hermana", "answer": "Abierto desde 2012. Más de 90.000 tazas al año. 4,1 de 5 en 300 reseñas.", "source": "informe"},
    {"kind": "fact", "collection": "operaciones", "question": "colores de la marca de la cadena hermana", "type": "brand", "answer": "Fondo blanco #FFFFFF, azul #112233, acento verde #22AA66.", "source": "manual de marca"},
    {"kind": "fact", "collection": "operaciones", "question": "tipografías de la marca de la cadena hermana", "type": "brand", "answer": "Títulos en Playfair Display, texto en Roboto.", "source": "manual de marca"},
    {"kind": "fact", "collection": "imagenes", "question": "foto de la fachada del almacén", "type": "image", "answer": "Portón metálico gris con rótulo de entregas.", "attrs": {"file": "img/almacen.jpg", "width": "1200", "height": "800", "uso": "interno", "alt": "Portón del almacén central"}, "source": "img/almacen.jpg"},
    {"kind": "fact", "collection": "imagenes", "question": "foto del espresso de la cadena hermana", "type": "image", "answer": "Taza negra con espresso sobre mármol.", "attrs": {"file": "img/espresso_hermana.jpg", "width": "1200", "height": "1200", "uso": "carta", "alt": "Espresso en taza negra sobre mármol"}, "source": "img/espresso_hermana.jpg"},
]

rows = base + filler + traps
rng.shuffle(rows)
json.dump(rows, open(HERE / "negocio_grande.json", "w", encoding="utf-8"), ensure_ascii=False, indent=1)
print(len(rows), "rows,", (HERE / "negocio_grande.json").stat().st_size, "bytes")
