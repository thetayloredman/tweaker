#![allow(clippy::get_first)]

use super::{Filter, Filters};
use regex::Regex;
use rand::{rng, seq::IteratorRandom};

struct RegexFilter;
impl Filter for RegexFilter {
	fn name(&self) -> &'static str {
		"regex"
	}

	fn apply(&self, string: String, args: Vec<String>) -> crate::Result<String> {
		let regex = args.get(0).ok_or("Missing regex")?;
		let regex = Regex::new(regex)?;
		let replace = args.get(1).cloned().unwrap_or_default();
		Ok(regex.replace(&string, replace).to_string())
	}
}

struct ParagraphsFilter;
impl Filter for ParagraphsFilter {
	fn name(&self) -> &'static str {
		"paragraphs"
	}

	fn apply(&self, string: String, args: Vec<String>) -> crate::Result<String> {
		let max_len = args.get(0).map(|s| s.parse::<usize>().unwrap_or(120)).unwrap_or(120);
		let mut llen = 0;
		let mut out = String::new();
		for line in string.lines() {
			out.push_str(line);
			out.push(' ');
			llen += line.len();
			if llen > max_len && out.ends_with('.') {
				llen = 0;
				out.push_str("\n\n");
			}
		}
		Ok(out)
	}
}

struct LowercaseFilter;
impl Filter for LowercaseFilter {
	fn name(&self) -> &'static str {
		"lowercase"
	}

	fn apply(&self, string: String, _args: Vec<String>) -> crate::Result<String> {
		Ok(string.to_lowercase())
	}
}

struct UppercaseFilter;
impl Filter for UppercaseFilter {
	fn name(&self) -> &'static str {
		"uppercase"
	}

	fn apply(&self, string: String, _args: Vec<String>) -> crate::Result<String> {
		Ok(string.to_uppercase())
	}
}

struct CleanEndingFilter;
impl Filter for CleanEndingFilter {
	fn name(&self) -> &'static str {
		"clean_ending"
	}

	fn apply(&self, string: String, args: Vec<String>) -> crate::Result<String> {
		let target_endings = args.get(0).cloned().unwrap_or(".".to_string());
		let clip_to = args.get(1).cloned().unwrap_or(".,".to_string());
		let replace_ending = target_endings.chars()
			.choose(&mut rng()).unwrap_or('.').to_string();

		let regex = format!("[{clip_to}][^{clip_to}]+[^{target_endings}]$");
		let regex = Regex::new(&regex)?;
		Ok(regex.replace(&string, replace_ending).to_string())
	}
}

struct MatchPairsFilter;
impl Filter for MatchPairsFilter {
	fn name(&self) -> &'static str {
		"match_pairs"
	}

	fn apply(&self, mut string: String, args: Vec<String>) -> crate::Result<String> {
		let start = args.get(0).cloned().unwrap_or("\"".to_string());
		let count_start = string.match_indices(&start).count();

		if let Some(end) = args.get(1) {
			let count_end = string.match_indices(end).count();
			let add = count_start.saturating_sub(count_end);
			if add > 0 {
				string.push_str(&end.repeat(add));
			}
		}
		else if count_start % 2 != 0 {
			string.push_str(&start);
		}

		Ok(string)
	}
}

struct ControlCharsFilter;
impl Filter for ControlCharsFilter {
	fn name(&self) -> &'static str {
		"controlchars"
	}

	fn apply(&self, string: String, _args: Vec<String>) -> crate::Result<String> {
		let filtered = string.chars()
			.filter(|c| !c.is_control() || *c == '\n');
		let filtered = String::from_iter(filtered);
		Ok(filtered)
	}
}

pub(crate) fn register_filters(filters: &mut Filters) {
	filters.register(RegexFilter);
	filters.register(ParagraphsFilter);
	filters.register(LowercaseFilter);
	filters.register(UppercaseFilter);
	filters.register(CleanEndingFilter);
	filters.register(MatchPairsFilter);
	filters.register(ControlCharsFilter);
}
