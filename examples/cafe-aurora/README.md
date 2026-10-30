# Ejemplo: cuatro agentes arman la web de un negocio

Un negocio local (el Café Aurora, de Miraflores, ficticio) quiere una página de
inicio. Cuatro agentes pequeños se reparten el trabajo. Ninguno recibe la
información del negocio pegada en su prompt: todos la **consultan** en una base
FDS, y las fotos nunca pasan por el modelo.

## Qué resuelve

| Sin FDS | Con FDS |
|---|---|
| Los precios y el horario viven en el prompt de cada agente; si cambian, hay que tocar cuatro prompts | Viven en un solo archivo; se cambian una vez |
| Un modelo pequeño inventa un precio o una ruta de imagen | Sale de un registro con su fuente y quién lo verificó; si no existe, la consulta responde `none` o `weak` |
| El modelo recibe los bytes de las fotos o los inventa | Recibe la descripción, el `alt` y el nombre del archivo; copia la referencia, no la imagen |
| Cada agente carga las mismas instrucciones cada vez | Las carga una vez; después son 15 palabras para confirmar que no cambiaron |

## Los archivos

| Archivo | Qué es |
|---|---|
| `negocio.json` | 18 registros: datos del negocio, marca, cuatro fotos y tres guías de método |
| `brief.md` | Las reglas comunes de los cuatro agentes (un `AGENTS.md`); se vuelve un *contexto* |
| `simular_agentes.py` | Ejecuta, sin ningún modelo, las consultas que haría cada agente y mide qué recibe |

## Paso 1: construir la base

Desde esta carpeta, con `fds` en el `PATH` (o con su ruta completa):

```
fds --db cafe-aurora.fds init cafe-aurora
fds --db cafe-aurora.fds import negocio.json
fds --db cafe-aurora.fds context import brief.md --name "Sitio web de Café Aurora"
```

Resultado real:

```
new 18 · replaced 0 · unchanged 0 · duplicates 0 · errors 0
Context `Sitio web de Café Aurora` loaded from brief.md; agents get it with fds_context, and pay for it once.

records      19  (15 facts · 3 procedures · 0 knowledge · 1 contexts)
types        brand (3), image (4), method (3), stats (1)
collections  default (1), imagenes (4), marca (3), negocio (8), web (3)
```

Son 8 KB. Para ver qué sabe: `fds --db cafe-aurora.fds ls`.

## Paso 2: qué ve un agente

Una foto es un registro con tipo `image`. El agente recibe esto, y **no** el
archivo:

```
$ fds --db cafe-aurora.fds query "foto de la fachada del local"
confidence: exact

[fact/image] 81e57599  foto de la fachada del local
    Fachada de esquina con toldo naranja y mesas en la vereda, a media mañana.
    — attrs: alt=Fachada del Café Aurora con toldo naranja y mesas en la vereda · file=img/fachada.jpg · height=1067 · uso=portada · width=1600
    — source: img/fachada.jpg · verified: human 2026-10-15 · match: exact (1)
```

Con eso, el agente escribe `<img src="img/fachada.jpg" width="1600" height="1067"
alt="Fachada del Café Aurora con toldo naranja y mesas en la vereda">` sin
haber visto un solo píxel. Un paso normal de la construcción (un `copy`) lleva
los archivos a la carpeta de la web.

Lo mismo con una guía de método: el agente de estructura pide *"armar la página
de inicio de un negocio local"* y recibe las cuatro secciones con su regla.

## Paso 3: conectar los agentes (solo lectura)

La primera versión es de **consulta**: los agentes leen y no escriben. El MCP
de `fds` ya es de solo lectura salvo que se pase `--write`.

En Claude Code:

```
claude mcp add fds -- C:\ruta\a\fds.exe --db C:\ruta\a\cafe-aurora.fds mcp
```

O, en un archivo de configuración de MCP:

```json
{ "mcpServers": { "fds": {
    "command": "C:\\ruta\\a\\fds.exe",
    "args": ["--db", "C:\\ruta\\a\\cafe-aurora.fds", "mcp"] } } }
```

Las herramientas que ve cada agente: `fds_query`, `fds_contexts`, `fds_context`
y `fds_status`.

## Paso 4: los cuatro agentes

Todos empiezan igual: *"Carga el contexto «Sitio web de Café Aurora» con
`fds_context`."* Después, cada uno:

| Agente | Su tarea | Qué consulta |
|---|---|---|
| **estructura** | Define las secciones y el HTML base | El método para armar la página de inicio |
| **textos** | Escribe los textos de cada sección | Horario, carta de cafés, carta de pastelería, cifras del negocio |
| **imágenes** | Elige y coloca las fotos | Las cuatro fotos, con su `file` y su `alt` |
| **revisión** | Comprueba la página terminada | La lista de accesibilidad |

Un texto de arranque para cada uno, listo para pegar:

```
Eres el agente de ESTRUCTURA. Carga el contexto «Sitio web de Café Aurora» con
fds_context. Consulta con fds_query el método para armar la página de inicio.
Entrega un HTML con las secciones vacías y comentarios que digan qué va en cada
una. No escribas textos ni elijas fotos.
```

```
Eres el agente de TEXTOS. Carga el contexto «Sitio web de Café Aurora» con
fds_context. Consulta horario, carta de cafés, carta de pastelería y
estadísticas. Copia precios, horarios y cifras tal cual. Entrega solo los
textos, por sección.
```

```
Eres el agente de IMÁGENES. Carga el contexto «Sitio web de Café Aurora» con
fds_context. Consulta las cuatro fotos y asígnalas a las secciones según su
campo «uso». Entrega las etiquetas <img> con el file y el alt exactos del
registro.
```

```
Eres el agente de REVISIÓN. Carga el contexto «Sitio web de Café Aurora» con
fds_context. Consulta la lista de accesibilidad y revisa la página que te den.
Entrega una lista de problemas con la regla que incumple cada uno.
```

## Qué ahorra (medido, sin modelo)

`python simular_agentes.py` ejecuta esas consultas por MCP y cuenta lo que
recibe cada agente. Unidad: **palabras**, no tokens del modelo; ver el
apartado de límites.

| Agente | Contexto (1.ª vez) | Consultas | Total |
|---|---|---|---|
| estructura | 211 | 107 | 318 |
| textos | 211 | 170 | 381 |
| imágenes | 211 | 236 | 447 |
| revisión | 211 | 88 | 299 |
| **Los cuatro** | | | **1.445** |

Pegar la base entera en el prompt de cada agente serían **1.920** palabras. Con
solo 18 registros la diferencia es pequeña (un 25 % menos). Lo importante es
cómo crece:

- Lo que cuesta cada agente con FDS **casi no depende del tamaño de la base**:
  el contexto más lo que consulta.
- Lo que cuesta pegarla crece con cada registro nuevo.

Con unos 200 registros (una carta completa y un catálogo de fotos), lo segundo
serían unas 5.000 palabras *por agente*, y lo primero seguiría rondando las 400.
Esa extrapolación sale de 25 palabras por registro y no está medida; la prueba
con modelos reales la comprobará.

## Con fotos de verdad

Aquí las descripciones están escritas a mano (por eso dicen `verified: human`).
Con fotos reales, FDS puede describirlas con un modelo de visión local:

```
fds --db cafe-aurora.fds add img/ --lang es
```

Esas descripciones quedan marcadas `model:qwen2.5vl:3b` hasta que una persona
las revise. Necesita Ollama con un modelo de visión (`qwen2.5vl:3b`, unos 3 GB).

## Límites, dichos con franqueza

- **No se ha probado con modelos reales.** Lo medido aquí son las consultas y lo
  que devuelven, no cómo las usa Haiku. Esa prueba es el siguiente paso.
- **El contexto debe llevar solo lo que *todos* necesitan.** La primera versión
  de este ejemplo enlazaba todos los registros en el contexto y salía *más
  caro* que pegar la base (3.887 palabras contra 2.076): cada agente recibía todo
  y además volvía a consultarlo. Los datos que usa uno solo se consultan, no se
  cargan.
- **Cada registro trae su procedencia**, y eso cuesta palabras: con registros
  de 15 palabras, la línea de fuente y verificación pesa casi lo mismo que el
  dato. Para un agente que no la necesita, una salida compacta ahorraría más.
- **El español es el punto débil.** Consultas sin relación como *«tiene menú
  vegano»* devuelven `weak` con un registro cualquiera, no `none`. Por eso la
  regla del contexto pide tratar `weak` como «no lo encontré».
- **Las cifras están en palabras.** Un token de modelo no es una palabra; en
  español y en JSON hay más tokens que palabras.
