create table if not exists seed_marker(v text);
truncate seed_marker;
insert into seed_marker values ('pybase-1.0.0');
