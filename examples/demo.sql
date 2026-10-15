-- Demonstração do mate. Rode com:
--   cargo run -- demo.db < examples/demo.sql
-- (apague demo.db e demo.db-wal para rodar de novo do zero)

CREATE TABLE clientes (
    id     INTEGER PRIMARY KEY,
    nome   TEXT NOT NULL,
    email  TEXT UNIQUE,
    cidade TEXT
);

CREATE TABLE pedidos (
    id         INTEGER PRIMARY KEY,
    cliente_id INTEGER NOT NULL,
    produto    TEXT,
    valor      FLOAT
);

-- Índice secundário: acelera buscas por cliente nos pedidos.
CREATE INDEX pedidos_cliente ON pedidos (cliente_id);

INSERT INTO clientes VALUES
    (1, 'Ana',    'ana@ex.com',    'Recife'),
    (2, 'Bruno',  'bruno@ex.com',  'Natal'),
    (3, 'Carla',  'carla@ex.com',  'Recife'),
    (4, 'Davi',   NULL,            'Fortaleza');

INSERT INTO pedidos VALUES
    (100, 1, 'Teclado', 150.0),
    (101, 1, 'Mouse',    60.5),
    (102, 3, 'Monitor', 900.0),
    (103, 2, 'Cabo',     25.0),
    (104, 3, 'Mouse',    60.5);

-- Consulta simples com filtro e ordenação.
SELECT nome, cidade FROM clientes WHERE cidade = 'Recife' ORDER BY nome;

-- JOIN + agregação: quanto cada cliente gastou (LEFT JOIN inclui quem não comprou).
SELECT c.nome, COUNT(p.id) AS pedidos, COALESCE(SUM(p.valor), 0) AS total
FROM clientes c LEFT JOIN pedidos p ON p.cliente_id = c.id
GROUP BY c.nome
ORDER BY total DESC;

-- O planner escolhe o índice da chave primária...
EXPLAIN SELECT * FROM clientes WHERE id = 3;

-- ...e o índice secundário em intervalos também.
EXPLAIN SELECT produto FROM pedidos WHERE cliente_id >= 2;

-- Sem índice na coluna, é varredura sequencial.
EXPLAIN SELECT * FROM pedidos WHERE produto = 'Mouse';

-- Restrições: este INSERT falha (email repetido) e não deixa rastro.
INSERT INTO clientes VALUES (5, 'Eva', 'ana@ex.com', 'Natal');

-- Transação desfeita.
BEGIN;
DELETE FROM pedidos;
SELECT COUNT(*) AS pedidos_dentro_da_transacao FROM pedidos;
ROLLBACK;
SELECT COUNT(*) AS pedidos_depois_do_rollback FROM pedidos;

-- O catálogo também é uma tabela.
SELECT type, name, root FROM mate_schema;
