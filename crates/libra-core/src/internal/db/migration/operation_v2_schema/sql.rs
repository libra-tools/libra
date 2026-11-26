use std::{iter::Peekable, str::Chars};

use sea_orm::DbErr;

use super::schema_error;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Token {
    Word(String),
    QuotedIdentifier(String),
    Literal(String),
    Symbol(char),
}

pub(super) fn predicate(sql: &str) -> Result<Vec<Token>, DbErr> {
    let tokens = tokens(sql)?;
    let mut depth = 0_i64;
    for (position, token) in tokens.iter().enumerate() {
        match token {
            Token::Symbol('(') => depth += 1,
            Token::Symbol(')') => depth -= 1,
            Token::Word(word) if depth == 0 && word == "where" => {
                return Ok(tokens.into_iter().skip(position).collect());
            }
            Token::Word(_) | Token::QuotedIdentifier(_) | Token::Literal(_) | Token::Symbol(_) => {}
        }
    }
    Ok(Vec::new())
}

pub(super) fn tokens(sql: &str) -> Result<Vec<Token>, DbErr> {
    Ok(lex(sql)?
        .into_iter()
        .map(|token| match token {
            Token::QuotedIdentifier(word) => Token::Word(word),
            Token::Word(_) | Token::Literal(_) | Token::Symbol(_) => token,
        })
        .collect())
}

pub(super) fn validate_table_constraints(sql: &str) -> Result<(), DbErr> {
    // None of the owned canonical tables uses these clauses. PRAGMA table_xinfo
    // cannot reveal their changed write/conflict semantics; quoted data is not a clause.
    for token in lex(sql)? {
        if let Token::Word(word) = token
            && matches!(
                word.as_str(),
                "conflict"
                    | "check"
                    | "references"
                    | "collate"
                    | "strict"
                    | "without"
                    | "unique"
                    | "virtual"
            )
        {
            return Err(schema_error(
                "table definition",
                format!("noncanonical {word} clause is not supported"),
            ));
        }
    }
    Ok(())
}

fn lex(sql: &str) -> Result<Vec<Token>, DbErr> {
    let mut chars = sql.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(character) = chars.next() {
        match character {
            ' ' | '\t' | '\n' | '\r' | '\u{000c}' => {}
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for value in chars.by_ref() {
                    if value == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                skip_comment(&mut chars)?;
            }
            '\'' => tokens.push(Token::Literal(quoted(&mut chars, '\'', true)?)),
            '"' | '`' => tokens.push(Token::QuotedIdentifier(
                quoted(&mut chars, character, true)?.to_ascii_lowercase(),
            )),
            '[' => tokens.push(Token::QuotedIdentifier(
                quoted(&mut chars, ']', false)?.to_ascii_lowercase(),
            )),
            value if value.is_alphanumeric() || value == '_' || value == '$' => {
                let mut word = String::from(value);
                while chars
                    .peek()
                    .is_some_and(|next| next.is_alphanumeric() || *next == '_' || *next == '$')
                {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                tokens.push(Token::Word(word.to_ascii_lowercase()));
            }
            value => tokens.push(Token::Symbol(value)),
        }
    }
    // sqlite_schema may retain or omit a final statement terminator.
    while tokens.last() == Some(&Token::Symbol(';')) {
        tokens.pop();
    }
    Ok(tokens)
}

fn quoted(chars: &mut Peekable<Chars<'_>>, end: char, doubled: bool) -> Result<String, DbErr> {
    let mut value = String::new();
    while let Some(character) = chars.next() {
        if character == end {
            if doubled && chars.peek() == Some(&end) {
                chars.next();
                value.push(end);
            } else {
                return Ok(value);
            }
        } else {
            value.push(character);
        }
    }
    Err(schema_error("SQL definition", "unterminated quoted token"))
}

fn skip_comment(chars: &mut Peekable<Chars<'_>>) -> Result<(), DbErr> {
    while let Some(character) = chars.next() {
        if character == '*' && chars.peek() == Some(&'/') {
            chars.next();
            return Ok(());
        }
    }
    Err(schema_error("SQL definition", "unterminated block comment"))
}
